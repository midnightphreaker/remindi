//! Read-only, alias-contained condition adapters.

mod file_exists;
mod http_health;
mod observation_window;
mod tcp_reachable;

use std::{
    collections::BTreeMap,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use async_trait::async_trait;
use schemars::Schema;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use time::OffsetDateTime;
use tokio::{
    net::lookup_host,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;

use crate::clock::Clock;

pub use file_exists::{FileExistsAdapter, FileTarget};
pub use http_health::{HttpHealthAdapter, HttpTarget};
pub use observation_window::ObservationWindowAdapter;
pub use tcp_reachable::{TcpReachableAdapter, TcpTarget};

const ADAPTER_VERSION: &str = "1.0.0";
const MAX_SUMMARY_BYTES: usize = 256;

/// One of the four condition outcomes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    Satisfied,
    Unsatisfied,
    Unknown,
    Error,
}

/// Bounded non-secret adapter metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AdapterMetadata {
    pub adapter_version: &'static str,
    pub latency_ms: u64,
}

/// Read-only condition observation returned to the caller.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AdapterResult {
    pub status: AdapterStatus,
    pub observed_at: OffsetDateTime,
    pub summary: String,
    pub metadata: AdapterMetadata,
}

impl AdapterResult {
    fn new(
        status: AdapterStatus,
        observed_at: OffsetDateTime,
        summary: &'static str,
        started: Instant,
    ) -> Self {
        debug_assert!(summary.len() <= MAX_SUMMARY_BYTES);
        Self {
            status,
            observed_at,
            summary: summary.to_owned(),
            metadata: AdapterMetadata {
                adapter_version: ADAPTER_VERSION,
                latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            },
        }
    }
}

/// Safe adapter configuration or evaluation setup failure.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AdapterConfigError {
    #[error("adapter configuration is invalid")]
    Invalid,
    #[error("adapter target is outside its configured containment boundary")]
    OutsideBoundary,
    #[error("adapter destination is denied by network policy")]
    DestinationDenied,
}

/// Explicit administrator network policy attached to one configured alias.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NetworkPolicy {
    allow_private: bool,
}

impl NetworkPolicy {
    /// Denies loopback, private, link-local, multicast, and metadata destinations.
    #[must_use]
    pub const fn public_only() -> Self {
        Self {
            allow_private: false,
        }
    }

    /// Explicit local-owner override for a configured alias.
    #[must_use]
    pub const fn allow_private_for_admin() -> Self {
        Self {
            allow_private: true,
        }
    }
}

/// Common async contract for all condition adapters.
#[async_trait]
pub trait ConditionAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str {
        ADAPTER_VERSION
    }
    fn parameter_schema(&self) -> Schema;
    async fn evaluate(
        &self,
        params: Value,
        deadline: Instant,
        cancel: CancellationToken,
    ) -> AdapterResult;
}

/// Immutable registry containing exactly the four v1 adapters.
pub struct AdapterRegistry {
    adapters: BTreeMap<&'static str, Arc<dyn ConditionAdapter>>,
}

impl AdapterRegistry {
    /// Creates the startup registry with every adapter disabled.
    #[must_use]
    pub fn disabled(clock: Arc<dyn Clock>) -> Self {
        let adapters: BTreeMap<&'static str, Arc<dyn ConditionAdapter>> = BTreeMap::from([
            (
                "file_exists",
                Arc::new(FileExistsAdapter::disabled(Arc::clone(&clock)))
                    as Arc<dyn ConditionAdapter>,
            ),
            (
                "http_health",
                Arc::new(HttpHealthAdapter::disabled(Arc::clone(&clock))),
            ),
            (
                "observation_window_ended",
                Arc::new(ObservationWindowAdapter::disabled(Arc::clone(&clock))),
            ),
            (
                "tcp_reachable",
                Arc::new(TcpReachableAdapter::disabled(clock)),
            ),
        ]);
        Self { adapters }
    }

    /// Returns the registered names in stable order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.adapters.keys().copied().collect()
    }

    /// Looks up one registered adapter.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn ConditionAdapter>> {
        self.adapters.get(name).cloned()
    }
}

/// Rejects network destinations that are unsafe under `policy`.
pub fn validate_destination(
    address: IpAddr,
    policy: NetworkPolicy,
) -> Result<(), AdapterConfigError> {
    if policy.allow_private {
        return Ok(());
    }
    let denied = match address {
        IpAddr::V4(address) => denied_v4(address),
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                denied_v4(mapped)
            } else {
                address.is_loopback()
                    || address.is_unspecified()
                    || address.is_multicast()
                    || is_ipv6_unique_local(address)
                    || is_ipv6_link_local(address)
            }
        }
    };
    if denied {
        Err(AdapterConfigError::DestinationDenied)
    } else {
        Ok(())
    }
}

fn denied_v4(address: Ipv4Addr) -> bool {
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_unspecified()
        || address.is_broadcast()
        || address == Ipv4Addr::new(100, 100, 100, 200)
}

fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 128
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_ipv6_unique_local(address: Ipv6Addr) -> bool {
    address.segments()[0] & 0xfe00 == 0xfc00
}

fn is_ipv6_link_local(address: Ipv6Addr) -> bool {
    address.segments()[0] & 0xffc0 == 0xfe80
}

async fn resolve_target(
    host: &str,
    port: u16,
    policy: NetworkPolicy,
) -> Result<Vec<SocketAddr>, AdapterConfigError> {
    let addresses = lookup_host((host, port))
        .await
        .map_err(|_| AdapterConfigError::Invalid)?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(AdapterConfigError::Invalid);
    }
    for address in &addresses {
        validate_destination(address.ip(), policy)?;
    }
    Ok(addresses)
}

async fn guarded<F>(
    clock: &dyn Clock,
    deadline: Instant,
    cancel: CancellationToken,
    operation: F,
) -> AdapterResult
where
    F: Future<Output = (AdapterStatus, &'static str)>,
{
    let started = Instant::now();
    tokio::select! {
        biased;
        () = cancel.cancelled() => AdapterResult::new(
            AdapterStatus::Unknown,
            clock.now(),
            "Adapter evaluation was cancelled.",
            started,
        ),
        () = sleep_until(deadline) => AdapterResult::new(
            AdapterStatus::Unknown,
            clock.now(),
            "Adapter evaluation timed out.",
            started,
        ),
        (status, summary) = operation => AdapterResult::new(
            status,
            clock.now(),
            summary,
            started,
        ),
    }
}

fn invalid_parameters(clock: &dyn Clock, started: Instant) -> AdapterResult {
    AdapterResult::new(
        AdapterStatus::Error,
        clock.now(),
        "Adapter parameters are invalid.",
        started,
    )
}
