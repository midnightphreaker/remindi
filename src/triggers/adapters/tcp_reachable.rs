use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use schemars::{JsonSchema, Schema, schema_for};
use serde::Deserialize;
use serde_json::Value;
use tokio::{net::TcpStream, time::Instant};
use tokio_util::sync::CancellationToken;

use super::{
    AdapterConfigError, AdapterResult, AdapterStatus, ConditionAdapter, NetworkPolicy, guarded,
    invalid_parameters, resolve_target, valid_alias,
};
use crate::clock::Clock;

/// One administrator-configured TCP alias.
#[derive(Clone, Debug)]
pub struct TcpTarget {
    host: String,
    port: u16,
    policy: NetworkPolicy,
}

impl TcpTarget {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        policy: NetworkPolicy,
    ) -> Result<Self, AdapterConfigError> {
        let host = host.into();
        if host.trim().is_empty()
            || host.len() > 253
            || port == 0
            || host.chars().any(char::is_control)
        {
            return Err(AdapterConfigError::Invalid);
        }
        Ok(Self { host, port, policy })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Parameters {
    target: String,
}

/// Connection-only TCP reachability adapter.
pub struct TcpReachableAdapter {
    enabled: bool,
    aliases: HashMap<String, TcpTarget>,
    clock: Arc<dyn Clock>,
}

impl TcpReachableAdapter {
    #[must_use]
    pub fn disabled(clock: Arc<dyn Clock>) -> Self {
        Self {
            enabled: false,
            aliases: HashMap::new(),
            clock,
        }
    }

    pub fn new(
        enabled: bool,
        aliases: HashMap<String, TcpTarget>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, AdapterConfigError> {
        if aliases.keys().any(|alias| !valid_alias(alias)) {
            return Err(AdapterConfigError::Invalid);
        }
        Ok(Self {
            enabled,
            aliases,
            clock,
        })
    }
}

#[async_trait]
impl ConditionAdapter for TcpReachableAdapter {
    fn name(&self) -> &'static str {
        "tcp_reachable"
    }

    fn parameter_schema(&self) -> Schema {
        schema_for!(Parameters)
    }

    async fn evaluate(
        &self,
        params: Value,
        deadline: Instant,
        cancel: CancellationToken,
    ) -> AdapterResult {
        let started = Instant::now();
        if !self.enabled {
            return AdapterResult::new(
                AdapterStatus::Unknown,
                self.clock.now(),
                "Adapter is disabled.",
                started,
            );
        }
        let parameters: Parameters = match serde_json::from_value(params) {
            Ok(parameters) => parameters,
            Err(_) => return invalid_parameters(self.clock.as_ref(), started),
        };
        if !valid_alias(&parameters.target) {
            return invalid_parameters(self.clock.as_ref(), started);
        }
        let Some(target) = self.aliases.get(&parameters.target).cloned() else {
            return AdapterResult::new(
                AdapterStatus::Unknown,
                self.clock.now(),
                "Configured target alias was not found.",
                started,
            );
        };
        guarded(self.clock.as_ref(), deadline, cancel, async move {
            let addresses = match resolve_target(&target.host, target.port, target.policy).await {
                Ok(addresses) => addresses,
                Err(_) => {
                    return (
                        AdapterStatus::Error,
                        "Configured target could not be safely resolved.",
                    );
                }
            };
            for address in addresses {
                if TcpStream::connect(address).await.is_ok() {
                    return (
                        AdapterStatus::Satisfied,
                        "Configured target accepted a TCP connection.",
                    );
                }
            }
            (
                AdapterStatus::Unsatisfied,
                "Configured target did not accept a TCP connection.",
            )
        })
        .await
    }
}
