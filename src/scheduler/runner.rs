use std::{sync::Arc, time::Duration as StdDuration};

use futures::{StreamExt, stream};
use thiserror::Error;
use time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    clock::Clock,
    db::{DatabaseError, DatabaseManager},
    remindi::{Actor, ActorType, Remindi, RemindiService, ServiceError, Trigger},
    triggers::{
        ConditionEvaluation,
        adapters::{AdapterRegistry, AdapterResult, AdapterStatus, ConditionAdapter},
    },
};

use super::{LeaseError, LeaseGuard, SchedulerLease};

/// Resolves condition adapters from an immutable configuration snapshot.
pub trait AdapterProvider: Send + Sync {
    /// Returns a configured adapter by its source-defined name.
    fn get(&self, name: &str) -> Option<Arc<dyn ConditionAdapter>>;
}

impl AdapterProvider for AdapterRegistry {
    fn get(&self, name: &str) -> Option<Arc<dyn ConditionAdapter>> {
        self.get(name)
    }
}

/// Runtime settings needed by one scheduler workload.
#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    pub poll_interval: StdDuration,
    pub lease_duration: StdDuration,
    pub adapter_timeout: StdDuration,
    pub adapter_concurrency: usize,
    pub candidate_batch_size: usize,
}

impl SchedulerConfig {
    fn validate(&self) -> Result<(), SchedulerError> {
        if self.poll_interval.is_zero()
            || self.lease_duration <= self.poll_interval.saturating_mul(2)
            || self.adapter_timeout.is_zero()
            || self.adapter_concurrency == 0
            || self.candidate_batch_size == 0
        {
            return Err(SchedulerError::InvalidConfiguration);
        }
        Ok(())
    }
}

/// Observable result of one deterministic polling iteration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PollReport {
    pub selected: usize,
    pub applied: usize,
    pub conflicts: usize,
    pub failures: usize,
}

/// Why a scheduler run loop ended cleanly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunExit {
    Cancelled,
    DesiredStopped,
    LeaseLost,
}

/// Scheduler configuration, lease, or persistence failure.
#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("scheduler configuration is invalid")]
    InvalidConfiguration,
    #[error("scheduler lease operation failed")]
    Lease(#[from] LeaseError),
    #[error("scheduler database operation failed")]
    Database(#[from] DatabaseError),
    #[error("scheduler service operation failed")]
    Service(#[from] ServiceError),
    #[error("scheduler desired state is invalid")]
    InvalidDesiredState,
}

/// Single-process scheduler workload. Application wiring should spawn `run`.
pub struct Scheduler {
    database: Arc<DatabaseManager>,
    service: Arc<RemindiService>,
    adapters: Arc<dyn AdapterProvider>,
    clock: Arc<dyn Clock>,
    config: SchedulerConfig,
    lease: SchedulerLease,
    actor: Actor,
}

impl Scheduler {
    /// Creates an isolated scheduler workload without starting it.
    pub fn new(
        database: Arc<DatabaseManager>,
        service: Arc<RemindiService>,
        adapters: Arc<dyn AdapterProvider>,
        clock: Arc<dyn Clock>,
        holder_id: impl Into<String>,
        config: SchedulerConfig,
    ) -> Result<Self, SchedulerError> {
        config.validate()?;
        let duration = Duration::try_from(config.lease_duration)
            .map_err(|_| SchedulerError::InvalidConfiguration)?;
        let holder_id = holder_id.into();
        let lease = SchedulerLease::new(Arc::clone(&database), holder_id.clone(), duration)?;
        Ok(Self {
            database,
            service,
            adapters,
            clock,
            config,
            lease,
            actor: Actor {
                actor_type: ActorType::Scheduler,
                actor_id: format!("scheduler:{holder_id}"),
                request_id: None,
            },
        })
    }

    /// Reads the persisted desired state owned by the workload controller.
    pub async fn desired_running(&self) -> Result<bool, SchedulerError> {
        let mut connection = self.database.connection().await?;
        let value: Option<String> = sqlx::query_scalar(
            "SELECT desired_state FROM service_runtime WHERE component = 'scheduler'",
        )
        .fetch_optional(connection.as_mut())
        .await
        .map_err(DatabaseError::from)?;
        match value.as_deref() {
            Some("running") => Ok(true),
            Some("stopped") => Ok(false),
            _ => Err(SchedulerError::InvalidDesiredState),
        }
    }

    /// Runs until cancellation, desired stop, or lease loss.
    pub async fn run(&self, cancel: CancellationToken) -> Result<RunExit, SchedulerError> {
        if !self.desired_running().await? {
            return Ok(RunExit::DesiredStopped);
        }
        let mut guard = self.lease.acquire(self.clock.now()).await?;
        let exit = loop {
            if cancel.is_cancelled() {
                break RunExit::Cancelled;
            }
            if !self.desired_running().await? {
                break RunExit::DesiredStopped;
            }
            match self.poll_once(&mut guard, cancel.child_token()).await {
                Ok(_) => {}
                Err(SchedulerError::Lease(LeaseError::Lost)) => break RunExit::LeaseLost,
                Err(error) => tracing::warn!(error = %error, "scheduler iteration failed"),
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => break RunExit::Cancelled,
                () = tokio::time::sleep(self.config.poll_interval) => {}
            }
        };
        match self.lease.release(&guard).await {
            Ok(()) | Err(LeaseError::Lost) => Ok(exit),
            Err(error) => Err(error.into()),
        }
    }

    /// Applies one bounded poll while preserving the supplied lease.
    pub async fn poll_once(
        &self,
        guard: &mut LeaseGuard,
        cancel: CancellationToken,
    ) -> Result<PollReport, SchedulerError> {
        let now = self.clock.now();
        if guard.renewal_due(now) {
            self.lease.renew(guard, now).await?;
        }
        let candidates = self
            .service
            .scheduler_candidates(now, self.config.candidate_batch_size)
            .await?;
        let selected = candidates.len();
        let concurrency = self.config.adapter_concurrency;
        let evaluations = stream::iter(candidates.into_iter().enumerate())
            .map(|(index, candidate)| {
                let adapters = Arc::clone(&self.adapters);
                let cancel = cancel.child_token();
                async move {
                    let evaluation = evaluate_condition(
                        adapters.as_ref(),
                        &candidate,
                        self.config.adapter_timeout,
                        cancel,
                    )
                    .await;
                    (index, candidate, evaluation)
                }
            })
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
        if cancel.is_cancelled() {
            return Ok(PollReport {
                selected,
                failures: selected,
                ..PollReport::default()
            });
        }

        self.lease.renew(guard, self.clock.now()).await?;
        let mut ordered = evaluations;
        ordered.sort_by_key(|(index, _, _)| *index);
        let mut report = PollReport {
            selected,
            ..PollReport::default()
        };
        for (_, candidate, evaluation) in ordered {
            let (condition, detail) = evaluation
                .map(|result| (map_status(result.status), Some(result.summary)))
                .unwrap_or((ConditionEvaluation::NotEvaluated, None));
            match self
                .service
                .apply_scheduler_evaluation(&self.actor, candidate, now, condition, detail)
                .await
            {
                Ok(()) => report.applied += 1,
                Err(ServiceError::VersionConflict { .. } | ServiceError::NotFound) => {
                    report.conflicts += 1;
                }
                Err(_) => report.failures += 1,
            }
        }
        Ok(report)
    }

    /// Acquires the lease for controlled one-shot polling or lifecycle tests.
    pub async fn acquire(&self) -> Result<LeaseGuard, SchedulerError> {
        self.lease
            .acquire(self.clock.now())
            .await
            .map_err(Into::into)
    }

    /// Releases a lease acquired through this scheduler.
    pub async fn release(&self, guard: &LeaseGuard) -> Result<(), SchedulerError> {
        self.lease.release(guard).await.map_err(Into::into)
    }
}

async fn evaluate_condition(
    adapters: &dyn AdapterProvider,
    candidate: &Remindi,
    timeout: StdDuration,
    cancel: CancellationToken,
) -> Option<AdapterResult> {
    let Trigger::Condition {
        adapter,
        parameters,
        ..
    } = &candidate.trigger
    else {
        return None;
    };
    let Some(adapter) = adapters.get(adapter) else {
        return Some(AdapterResult {
            status: AdapterStatus::Unknown,
            observed_at: candidate.updated_at,
            summary: "Configured condition adapter is unavailable.".to_owned(),
            metadata: crate::triggers::adapters::AdapterMetadata {
                adapter_version: "unavailable",
                latency_ms: 0,
            },
        });
    };
    adapter
        .evaluate(parameters.clone(), Instant::now() + timeout, cancel)
        .await
        .into()
}

const fn map_status(status: AdapterStatus) -> ConditionEvaluation {
    match status {
        AdapterStatus::Satisfied => ConditionEvaluation::Satisfied,
        AdapterStatus::Unsatisfied => ConditionEvaluation::Unsatisfied,
        AdapterStatus::Unknown => ConditionEvaluation::Unknown,
        AdapterStatus::Error => ConditionEvaluation::Error,
    }
}
