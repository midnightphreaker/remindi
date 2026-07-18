use std::{
    collections::HashMap,
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use schemars::{JsonSchema, Schema, schema_for};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    AdapterConfigError, AdapterResult, AdapterStatus, ConditionAdapter, guarded,
    invalid_parameters, valid_alias,
};
use crate::clock::Clock;

/// One absolute administrator-configured path alias.
#[derive(Clone)]
pub struct FileTarget {
    path: PathBuf,
}

impl FileTarget {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, AdapterConfigError> {
        let path = path.into();
        if !path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(AdapterConfigError::Invalid);
        }
        Ok(Self { path })
    }
}

impl fmt::Debug for FileTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FileTarget(REDACTED)")
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Parameters {
    path_alias: String,
}

/// Metadata-only filesystem existence adapter.
pub struct FileExistsAdapter {
    enabled: bool,
    roots: Vec<PathBuf>,
    aliases: HashMap<String, FileTarget>,
    clock: Arc<dyn Clock>,
}

impl FileExistsAdapter {
    #[must_use]
    pub fn disabled(clock: Arc<dyn Clock>) -> Self {
        Self {
            enabled: false,
            roots: vec![],
            aliases: HashMap::new(),
            clock,
        }
    }

    pub fn new(
        enabled: bool,
        roots: Vec<PathBuf>,
        aliases: HashMap<String, FileTarget>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, AdapterConfigError> {
        let roots = roots
            .iter()
            .map(|root| std::fs::canonicalize(root).map_err(|_| AdapterConfigError::Invalid))
            .collect::<Result<Vec<_>, _>>()?;
        if aliases.keys().any(|alias| !valid_alias(alias)) {
            return Err(AdapterConfigError::Invalid);
        }
        for target in aliases.values() {
            validate_containment(&target.path, &roots)?;
        }
        Ok(Self {
            enabled,
            roots,
            aliases,
            clock,
        })
    }
}

#[async_trait]
impl ConditionAdapter for FileExistsAdapter {
    fn name(&self) -> &'static str {
        "file_exists"
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
        if !valid_alias(&parameters.path_alias) {
            return invalid_parameters(self.clock.as_ref(), started);
        }
        let Some(target) = self.aliases.get(&parameters.path_alias).cloned() else {
            return AdapterResult::new(
                AdapterStatus::Unknown,
                self.clock.now(),
                "Configured path alias was not found.",
                started,
            );
        };
        let roots = self.roots.clone();
        guarded(self.clock.as_ref(), deadline, cancel, async move {
            match contained_metadata(&target.path, &roots).await {
                Ok(true) => (AdapterStatus::Satisfied, "Configured path exists."),
                Ok(false) => (
                    AdapterStatus::Unsatisfied,
                    "Configured path does not exist.",
                ),
                Err(_) => (
                    AdapterStatus::Error,
                    "Configured path failed containment validation.",
                ),
            }
        })
        .await
    }
}

fn validate_containment(path: &Path, roots: &[PathBuf]) -> Result<(), AdapterConfigError> {
    let resolved = closest_existing(path)?;
    if roots.iter().any(|root| resolved.starts_with(root)) {
        Ok(())
    } else {
        Err(AdapterConfigError::OutsideBoundary)
    }
}

fn closest_existing(path: &Path) -> Result<PathBuf, AdapterConfigError> {
    let mut candidate = path;
    loop {
        match std::fs::canonicalize(candidate) {
            Ok(resolved) => return Ok(resolved),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or(AdapterConfigError::Invalid)?;
            }
            Err(_) => return Err(AdapterConfigError::Invalid),
        }
    }
}

async fn contained_metadata(path: &Path, roots: &[PathBuf]) -> Result<bool, AdapterConfigError> {
    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            validate_containment(path, roots)?;
            return Ok(false);
        }
        Err(_) => return Err(AdapterConfigError::Invalid),
    };
    if !roots.iter().any(|root| resolved.starts_with(root)) {
        return Err(AdapterConfigError::OutsideBoundary);
    }
    tokio::fs::metadata(resolved)
        .await
        .map(|_| true)
        .map_err(|_| AdapterConfigError::Invalid)
}
