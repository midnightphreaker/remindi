use std::sync::Arc;

use async_trait::async_trait;
use schemars::{JsonSchema, Schema, schema_for};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{AdapterResult, AdapterStatus, ConditionAdapter, guarded, invalid_parameters};
use crate::{
    clock::Clock,
    remindi::{DomainError, parse_timestamp},
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Parameters {
    window_end: String,
}

/// Pure time-comparison adapter.
pub struct ObservationWindowAdapter {
    enabled: bool,
    clock: Arc<dyn Clock>,
}

impl ObservationWindowAdapter {
    #[must_use]
    pub fn enabled(clock: Arc<dyn Clock>) -> Self {
        Self {
            enabled: true,
            clock,
        }
    }

    #[must_use]
    pub fn disabled(clock: Arc<dyn Clock>) -> Self {
        Self {
            enabled: false,
            clock,
        }
    }
}

#[async_trait]
impl ConditionAdapter for ObservationWindowAdapter {
    fn name(&self) -> &'static str {
        "observation_window_ended"
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
        let window_end = match parse_timestamp(&parameters.window_end) {
            Ok(window_end) => window_end,
            Err(DomainError::TimestampOffsetRequired | DomainError::InvalidTimestamp) => {
                return invalid_parameters(self.clock.as_ref(), started);
            }
            Err(_) => return invalid_parameters(self.clock.as_ref(), started),
        };
        let clock = Arc::clone(&self.clock);
        guarded(self.clock.as_ref(), deadline, cancel, async move {
            if clock.now() >= window_end {
                (AdapterStatus::Satisfied, "Observation window ended.")
            } else {
                (
                    AdapterStatus::Unsatisfied,
                    "Observation window has not ended.",
                )
            }
        })
        .await
    }
}
