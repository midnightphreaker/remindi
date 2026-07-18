use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{
    clock::Clock,
    db::DatabaseManager,
    scheduler::AdapterProvider,
    triggers::adapters::{
        ConditionAdapter, FileExistsAdapter, FileTarget, HttpHealthAdapter, HttpTarget,
        NetworkPolicy, ObservationWindowAdapter, TcpReachableAdapter, TcpTarget,
    },
};

use super::AdminError;

const ADAPTER_NAMES: &[&str] = &[
    "file_exists",
    "http_health",
    "observation_window_ended",
    "tcp_reachable",
];

/// Administrator-defined HTTPS alias.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpAliasConfiguration {
    pub url: String,
    pub expected_statuses: Vec<u16>,
    pub max_response_bytes: usize,
    pub expected_content_type: Option<String>,
    pub allow_redirects: bool,
    pub allow_private: bool,
}

/// Administrator-defined TCP alias.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TcpAliasConfiguration {
    pub host: String,
    pub port: u16,
    pub allow_private: bool,
}

/// Typed configuration for exactly one of the four v1 adapters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdapterConfiguration {
    ObservationWindowEnded,
    HttpHealth {
        aliases: BTreeMap<String, HttpAliasConfiguration>,
    },
    TcpReachable {
        aliases: BTreeMap<String, TcpAliasConfiguration>,
    },
    FileExists {
        roots: Vec<PathBuf>,
        aliases: BTreeMap<String, PathBuf>,
    },
}

/// Persisted adapter configuration returned to an authenticated admin API.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdapterConfigView {
    pub adapter_name: String,
    pub enabled: bool,
    pub configuration: AdapterConfiguration,
    pub version: i64,
    pub updated_at: String,
    pub updated_by: String,
}

#[derive(Clone)]
pub(super) struct AdapterSnapshot {
    adapters: BTreeMap<&'static str, Arc<dyn ConditionAdapter>>,
}

/// Lock-protected immutable adapter snapshot compatible with the scheduler.
///
/// Each publication replaces one complete snapshot under a single write lock.
#[derive(Clone)]
pub struct PublishedAdapters {
    active: Arc<RwLock<Arc<AdapterSnapshot>>>,
}

impl PublishedAdapters {
    fn new(snapshot: AdapterSnapshot) -> Self {
        Self {
            active: Arc::new(RwLock::new(Arc::new(snapshot))),
        }
    }

    pub(super) fn publish(&self, snapshot: AdapterSnapshot) -> Result<(), AdminError> {
        let mut active = self.active.write().map_err(|_| AdminError::Database)?;
        *active = Arc::new(snapshot);
        Ok(())
    }
}

impl AdapterProvider for PublishedAdapters {
    fn get(&self, name: &str) -> Option<Arc<dyn ConditionAdapter>> {
        self.active
            .read()
            .ok()
            .and_then(|snapshot| snapshot.adapters.get(name).cloned())
    }
}

pub(super) async fn load(
    database: &DatabaseManager,
    clock: Arc<dyn Clock>,
) -> Result<(Vec<AdapterConfigView>, PublishedAdapters), AdminError> {
    let rows = list(database).await?;
    let snapshot = build_snapshot(&rows, clock)?;
    Ok((rows, PublishedAdapters::new(snapshot)))
}

pub(super) async fn list(database: &DatabaseManager) -> Result<Vec<AdapterConfigView>, AdminError> {
    let mut connection = database
        .connection()
        .await
        .map_err(|_| AdminError::Database)?;
    let rows = sqlx::query(
        "SELECT adapter_name, enabled, config_json, version, updated_at, updated_by \
         FROM adapter_configs ORDER BY adapter_name",
    )
    .fetch_all(connection.as_mut())
    .await
    .map_err(|_| AdminError::Database)?;

    let configs = rows
        .into_iter()
        .map(|row| {
            let adapter_name: String = row.get("adapter_name");
            let config_json: String = row.get("config_json");
            Ok(AdapterConfigView {
                configuration: decode(&adapter_name, &config_json)?,
                adapter_name,
                enabled: row.get::<i64, _>("enabled") == 1,
                version: row.get("version"),
                updated_at: row.get("updated_at"),
                updated_by: row.get("updated_by"),
            })
        })
        .collect::<Result<Vec<_>, AdminError>>()?;
    if configs.len() != ADAPTER_NAMES.len()
        || configs
            .iter()
            .map(|config| config.adapter_name.as_str())
            .ne(ADAPTER_NAMES.iter().copied())
    {
        return Err(AdminError::Validation);
    }
    Ok(configs)
}

pub(super) fn candidate(
    current: &[AdapterConfigView],
    adapter_name: &str,
    enabled: bool,
    configuration: AdapterConfiguration,
    actor_id: &str,
    occurred_at: String,
    clock: Arc<dyn Clock>,
) -> Result<(AdapterConfigView, AdapterSnapshot), AdminError> {
    validate_name_matches(adapter_name, &configuration)?;
    let mut configs = current.to_vec();
    let selected = configs
        .iter_mut()
        .find(|config| config.adapter_name == adapter_name)
        .ok_or(AdminError::Validation)?;
    selected.enabled = enabled;
    selected.configuration = configuration;
    selected.version += 1;
    selected.updated_at = occurred_at;
    selected.updated_by = actor_id.to_owned();
    let updated = selected.clone();
    let snapshot = build_snapshot(&configs, clock)?;
    Ok((updated, snapshot))
}

pub(super) fn encoded(configuration: &AdapterConfiguration) -> Result<String, AdminError> {
    serde_json::to_string(configuration).map_err(|_| AdminError::Validation)
}

pub(super) fn is_known(name: &str) -> bool {
    ADAPTER_NAMES.contains(&name)
}

fn decode(name: &str, encoded: &str) -> Result<AdapterConfiguration, AdminError> {
    if encoded == "{}" {
        return match name {
            "observation_window_ended" => Ok(AdapterConfiguration::ObservationWindowEnded),
            "http_health" => Ok(AdapterConfiguration::HttpHealth {
                aliases: BTreeMap::new(),
            }),
            "tcp_reachable" => Ok(AdapterConfiguration::TcpReachable {
                aliases: BTreeMap::new(),
            }),
            "file_exists" => Ok(AdapterConfiguration::FileExists {
                roots: vec![],
                aliases: BTreeMap::new(),
            }),
            _ => Err(AdminError::Validation),
        };
    }
    let configuration = serde_json::from_str(encoded).map_err(|_| AdminError::Validation)?;
    validate_name_matches(name, &configuration)?;
    Ok(configuration)
}

fn validate_name_matches(
    name: &str,
    configuration: &AdapterConfiguration,
) -> Result<(), AdminError> {
    let matches = matches!(
        (name, configuration),
        (
            "observation_window_ended",
            AdapterConfiguration::ObservationWindowEnded
        ) | ("http_health", AdapterConfiguration::HttpHealth { .. })
            | ("tcp_reachable", AdapterConfiguration::TcpReachable { .. })
            | ("file_exists", AdapterConfiguration::FileExists { .. })
    );
    if matches {
        Ok(())
    } else {
        Err(AdminError::Validation)
    }
}

fn build_snapshot(
    configs: &[AdapterConfigView],
    clock: Arc<dyn Clock>,
) -> Result<AdapterSnapshot, AdminError> {
    let mut adapters = BTreeMap::new();
    for config in configs {
        let (name, adapter) = build_adapter(config, Arc::clone(&clock))?;
        if adapters.insert(name, adapter).is_some() {
            return Err(AdminError::Validation);
        }
    }
    if adapters.len() != ADAPTER_NAMES.len() {
        return Err(AdminError::Validation);
    }
    Ok(AdapterSnapshot { adapters })
}

fn build_adapter(
    config: &AdapterConfigView,
    clock: Arc<dyn Clock>,
) -> Result<(&'static str, Arc<dyn ConditionAdapter>), AdminError> {
    match &config.configuration {
        AdapterConfiguration::ObservationWindowEnded => {
            validate_name_matches(&config.adapter_name, &config.configuration)?;
            let adapter: Arc<dyn ConditionAdapter> = if config.enabled {
                Arc::new(ObservationWindowAdapter::enabled(clock))
            } else {
                Arc::new(ObservationWindowAdapter::disabled(clock))
            };
            Ok(("observation_window_ended", adapter))
        }
        AdapterConfiguration::HttpHealth { aliases } => {
            validate_name_matches(&config.adapter_name, &config.configuration)?;
            let targets = aliases
                .iter()
                .map(|(alias, target)| {
                    let policy = network_policy(target.allow_private);
                    HttpTarget::new(
                        &target.url,
                        target.expected_statuses.clone(),
                        target.max_response_bytes,
                        target.expected_content_type.clone(),
                        target.allow_redirects,
                        policy,
                    )
                    .map(|target| (alias.clone(), target))
                    .map_err(|_| AdminError::Validation)
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
            let adapter = HttpHealthAdapter::new(config.enabled, targets, clock)
                .map_err(|_| AdminError::Validation)?;
            Ok(("http_health", Arc::new(adapter)))
        }
        AdapterConfiguration::TcpReachable { aliases } => {
            validate_name_matches(&config.adapter_name, &config.configuration)?;
            let targets = aliases
                .iter()
                .map(|(alias, target)| {
                    TcpTarget::new(
                        &target.host,
                        target.port,
                        network_policy(target.allow_private),
                    )
                    .map(|target| (alias.clone(), target))
                    .map_err(|_| AdminError::Validation)
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
            let adapter = TcpReachableAdapter::new(config.enabled, targets, clock)
                .map_err(|_| AdminError::Validation)?;
            Ok(("tcp_reachable", Arc::new(adapter)))
        }
        AdapterConfiguration::FileExists { roots, aliases } => {
            validate_name_matches(&config.adapter_name, &config.configuration)?;
            let targets = aliases
                .iter()
                .map(|(alias, path)| {
                    FileTarget::new(path)
                        .map(|target| (alias.clone(), target))
                        .map_err(|_| AdminError::Validation)
                })
                .collect::<Result<HashMap<_, _>, _>>()?;
            let adapter = FileExistsAdapter::new(config.enabled, roots.clone(), targets, clock)
                .map_err(|_| AdminError::Validation)?;
            Ok(("file_exists", Arc::new(adapter)))
        }
    }
}

fn network_policy(allow_private: bool) -> NetworkPolicy {
    if allow_private {
        NetworkPolicy::allow_private_for_admin()
    } else {
        NetworkPolicy::public_only()
    }
}
