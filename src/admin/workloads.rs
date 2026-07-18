use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, MutexGuard};

use crate::{
    clock::Clock,
    db::{DatabaseError, DatabaseManager},
    mcp::server::McpWorkload,
    remindi::canonical_timestamp,
    scheduler::SchedulerWorkload,
};

const MAX_LAST_ERROR_BYTES: usize = 512;

/// A persisted workload or the virtual ordered pair.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadComponent {
    Mcp,
    Scheduler,
    All,
}

impl WorkloadComponent {
    const fn persisted_name(self) -> Option<&'static str> {
        match self {
            Self::Mcp => Some("mcp"),
            Self::Scheduler => Some("scheduler"),
            Self::All => None,
        }
    }
}

/// An administrative lifecycle request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadAction {
    Start,
    Stop,
    Restart,
}

/// Persisted intent for one workload.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
    Running,
    Stopped,
}

impl DesiredState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
        }
    }
}

/// Truthful in-memory lifecycle state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActualState {
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

/// Safe workload state returned to the authenticated administration layer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkloadStatus {
    pub component: WorkloadComponent,
    pub desired: DesiredState,
    pub actual: ActualState,
    pub last_error: Option<String>,
}

#[derive(Clone)]
struct MutableStatus {
    desired: DesiredState,
    actual: ActualState,
    last_error: Option<String>,
}

struct ControllerState {
    mcp: MutableStatus,
    scheduler: MutableStatus,
}

/// Runtime seam used by the concrete MCP and scheduler lifecycle owners.
#[async_trait]
pub trait WorkloadRuntime: Send + Sync {
    fn is_running(&self) -> bool;
    async fn start(&self) -> Result<(), String>;
    async fn stop(&self) -> Result<(), String>;
}

#[async_trait]
impl WorkloadRuntime for McpWorkload {
    fn is_running(&self) -> bool {
        self.is_running()
    }

    async fn start(&self) -> Result<(), String> {
        self.start().map_err(|error| error.to_string())
    }

    async fn stop(&self) -> Result<(), String> {
        self.stop().map_err(|error| error.to_string())
    }
}

#[async_trait]
impl WorkloadRuntime for SchedulerWorkload {
    fn is_running(&self) -> bool {
        self.is_running()
    }

    async fn start(&self) -> Result<(), String> {
        self.start().await.map_err(|error| error.to_string())
    }

    async fn stop(&self) -> Result<(), String> {
        self.stop().await.map_err(|error| error.to_string())
    }
}

/// Safe persistence or lifecycle failure.
#[derive(Debug, Error)]
pub enum WorkloadError {
    #[error("another workload transition is in progress")]
    TransitionConflict,
    #[error("workload persistence failed")]
    Database(#[from] DatabaseError),
    #[error("persisted workload state is invalid")]
    InvalidPersistedState,
    #[error("workload state is unavailable")]
    StateUnavailable,
    #[error("{component:?} workload transition failed")]
    TransitionFailed {
        component: WorkloadComponent,
        message: String,
    },
}

/// Serializes and reconciles all in-process workload lifecycle changes.
pub struct WorkloadController {
    database: Arc<DatabaseManager>,
    clock: Arc<dyn Clock>,
    mcp: Arc<dyn WorkloadRuntime>,
    scheduler: Arc<dyn WorkloadRuntime>,
    transition: Mutex<()>,
    state: RwLock<ControllerState>,
}

/// Holds the lifecycle transition lock while restore owns database maintenance.
pub struct WorkloadMaintenance<'a> {
    controller: &'a WorkloadController,
    _transition: MutexGuard<'a, ()>,
}

impl WorkloadController {
    /// Builds and reconciles the concrete MCP and scheduler workloads from persisted intent.
    pub async fn new(
        database: Arc<DatabaseManager>,
        clock: Arc<dyn Clock>,
        mcp: Arc<McpWorkload>,
        scheduler: Arc<SchedulerWorkload>,
    ) -> Result<Self, WorkloadError> {
        Self::from_runtimes(database, clock, mcp, scheduler).await
    }

    /// Builds a controller from lifecycle runtimes, allowing deterministic E2E probes.
    pub async fn from_runtimes(
        database: Arc<DatabaseManager>,
        clock: Arc<dyn Clock>,
        mcp: Arc<dyn WorkloadRuntime>,
        scheduler: Arc<dyn WorkloadRuntime>,
    ) -> Result<Self, WorkloadError> {
        let (mcp_desired, scheduler_desired) = load_desired(&database).await?;
        let controller = Self {
            database,
            clock,
            mcp: Arc::clone(&mcp),
            scheduler: Arc::clone(&scheduler),
            transition: Mutex::new(()),
            state: RwLock::new(ControllerState {
                mcp: initial_status(mcp_desired, mcp.is_running()),
                scheduler: initial_status(scheduler_desired, scheduler.is_running()),
            }),
        };
        let _guard = controller.transition.lock().await;
        controller
            .reconcile_component(WorkloadComponent::Mcp, mcp_desired)
            .await?;
        controller
            .reconcile_component(WorkloadComponent::Scheduler, scheduler_desired)
            .await?;
        drop(_guard);
        Ok(controller)
    }

    /// Returns a stable MCP-then-scheduler status snapshot.
    #[must_use]
    pub fn status(&self) -> Vec<WorkloadStatus> {
        let Ok(mut state) = self.state.write() else {
            return Vec::new();
        };
        refresh_runtime_state(&mut state.mcp, self.mcp.is_running());
        refresh_runtime_state(&mut state.scheduler, self.scheduler.is_running());
        vec![
            snapshot(WorkloadComponent::Mcp, &state.mcp),
            snapshot(WorkloadComponent::Scheduler, &state.scheduler),
        ]
    }

    /// Persists intent atomically, then performs the ordered in-process transition.
    pub async fn transition(
        &self,
        component: WorkloadComponent,
        action: WorkloadAction,
        actor_id: &str,
        _request_id: Option<&str>,
    ) -> Result<Vec<WorkloadStatus>, WorkloadError> {
        let _guard = self
            .transition
            .try_lock()
            .map_err(|_| WorkloadError::TransitionConflict)?;
        let desired = match action {
            WorkloadAction::Stop => DesiredState::Stopped,
            WorkloadAction::Start | WorkloadAction::Restart => DesiredState::Running,
        };
        self.persist_desired(component, desired, actor_id).await?;
        self.set_desired(component, desired)?;
        for selected in selected_components(component) {
            self.apply_action(*selected, action).await?;
        }
        Ok(self.status())
    }

    /// Stops active tasks for process shutdown without changing persisted intent.
    pub async fn shutdown(&self) -> Result<(), WorkloadError> {
        let _guard = self.transition.lock().await;
        for component in selected_components(WorkloadComponent::All) {
            if self.runtime(*component).is_running() {
                self.apply_action(*component, WorkloadAction::Stop).await?;
            }
        }
        Ok(())
    }

    /// Stops MCP and scheduler without changing persisted desired state.
    pub async fn quiesce_for_maintenance(&self) -> Result<WorkloadMaintenance<'_>, WorkloadError> {
        let transition = self.transition.lock().await;
        let mut stopped = Vec::new();
        for component in selected_components(WorkloadComponent::All) {
            if self.runtime(*component).is_running() {
                if let Err(error) = self.apply_action(*component, WorkloadAction::Stop).await {
                    for stopped_component in stopped.into_iter().rev() {
                        let _ = self
                            .apply_action(stopped_component, WorkloadAction::Start)
                            .await;
                    }
                    return Err(error);
                }
                stopped.push(*component);
            }
        }
        Ok(WorkloadMaintenance {
            controller: self,
            _transition: transition,
        })
    }

    async fn persist_desired(
        &self,
        component: WorkloadComponent,
        desired: DesiredState,
        actor_id: &str,
    ) -> Result<(), WorkloadError> {
        let occurred_at = canonical_timestamp(self.clock.now())
            .map_err(|_| WorkloadError::InvalidPersistedState)?;
        let mut transaction = self.database.begin_immediate().await?;
        for selected in selected_components(component) {
            sqlx::query(
                "UPDATE service_runtime
                 SET desired_state = ?, version = version + 1, updated_at = ?, updated_by = ?
                 WHERE component = ?",
            )
            .bind(desired.as_str())
            .bind(&occurred_at)
            .bind(actor_id)
            .bind(
                selected
                    .persisted_name()
                    .expect("selected components are persisted"),
            )
            .execute(transaction.as_mut())
            .await
            .map_err(DatabaseError::from)?;
        }
        transaction.commit().await?;
        Ok(())
    }

    async fn reconcile_component(
        &self,
        component: WorkloadComponent,
        desired: DesiredState,
    ) -> Result<(), WorkloadError> {
        let runtime = self.runtime(component);
        match (desired, runtime.is_running()) {
            (DesiredState::Running, false) => {
                self.apply_action(component, WorkloadAction::Start).await
            }
            (DesiredState::Stopped, true) => {
                self.apply_action(component, WorkloadAction::Stop).await
            }
            (DesiredState::Running, true) => {
                self.set_actual(component, ActualState::Running, None)?;
                Ok(())
            }
            (DesiredState::Stopped, false) => {
                self.set_actual(component, ActualState::Stopped, None)?;
                Ok(())
            }
        }
    }

    async fn apply_action(
        &self,
        component: WorkloadComponent,
        action: WorkloadAction,
    ) -> Result<(), WorkloadError> {
        let runtime = self.runtime(component);
        let result = match action {
            WorkloadAction::Start => {
                self.set_actual(component, ActualState::Starting, None)?;
                runtime.start().await
            }
            WorkloadAction::Stop => {
                self.set_actual(component, ActualState::Stopping, None)?;
                runtime.stop().await
            }
            WorkloadAction::Restart => {
                self.set_actual(component, ActualState::Stopping, None)?;
                if let Err(error) = runtime.stop().await {
                    return self.fail(component, error);
                }
                self.set_actual(component, ActualState::Starting, None)?;
                runtime.start().await
            }
        };
        match result {
            Ok(()) => {
                let actual = match action {
                    WorkloadAction::Stop => ActualState::Stopped,
                    WorkloadAction::Start | WorkloadAction::Restart => ActualState::Running,
                };
                self.set_actual(component, actual, None)
            }
            Err(error) => self.fail(component, error),
        }
    }

    fn fail(&self, component: WorkloadComponent, error: String) -> Result<(), WorkloadError> {
        let bounded = bound_error(&error);
        self.set_actual(component, ActualState::Failed, Some(bounded.clone()))?;
        Err(WorkloadError::TransitionFailed {
            component,
            message: bounded,
        })
    }

    fn runtime(&self, component: WorkloadComponent) -> Arc<dyn WorkloadRuntime> {
        match component {
            WorkloadComponent::Mcp => Arc::clone(&self.mcp),
            WorkloadComponent::Scheduler => Arc::clone(&self.scheduler),
            WorkloadComponent::All => unreachable!("virtual component is expanded first"),
        }
    }

    fn set_desired(
        &self,
        component: WorkloadComponent,
        desired: DesiredState,
    ) -> Result<(), WorkloadError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| WorkloadError::StateUnavailable)?;
        for selected in selected_components(component) {
            status_mut(&mut state, *selected).desired = desired;
        }
        Ok(())
    }

    fn set_actual(
        &self,
        component: WorkloadComponent,
        actual: ActualState,
        last_error: Option<String>,
    ) -> Result<(), WorkloadError> {
        let mut state = self
            .state
            .write()
            .map_err(|_| WorkloadError::StateUnavailable)?;
        let status = status_mut(&mut state, component);
        status.actual = actual;
        status.last_error = last_error;
        Ok(())
    }
}

impl WorkloadMaintenance<'_> {
    /// Re-reads persisted intent from the active database and reconciles both workloads.
    pub async fn restart_from_persisted(&self) -> Result<Vec<WorkloadStatus>, WorkloadError> {
        let (mcp_desired, scheduler_desired) = load_desired(&self.controller.database).await?;
        self.controller
            .set_desired(WorkloadComponent::Mcp, mcp_desired)?;
        self.controller
            .set_desired(WorkloadComponent::Scheduler, scheduler_desired)?;
        self.controller
            .reconcile_component(WorkloadComponent::Mcp, mcp_desired)
            .await?;
        self.controller
            .reconcile_component(WorkloadComponent::Scheduler, scheduler_desired)
            .await?;
        Ok(self.controller.status())
    }

    /// Stops any workload that partially restarted while preserving persisted intent.
    pub async fn quiesce_again(&self) -> Result<(), WorkloadError> {
        for component in selected_components(WorkloadComponent::All) {
            if self.controller.runtime(*component).is_running() {
                self.controller
                    .apply_action(*component, WorkloadAction::Stop)
                    .await?;
            }
        }
        Ok(())
    }
}

async fn load_desired(
    database: &DatabaseManager,
) -> Result<(DesiredState, DesiredState), WorkloadError> {
    let mut connection = database.connection().await?;
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT component, desired_state FROM service_runtime ORDER BY component")
            .fetch_all(connection.as_mut())
            .await
            .map_err(DatabaseError::from)?;
    let mut mcp = None;
    let mut scheduler = None;
    for (component, desired) in rows {
        let desired = parse_desired(&desired)?;
        match component.as_str() {
            "mcp" => mcp = Some(desired),
            "scheduler" => scheduler = Some(desired),
            _ => return Err(WorkloadError::InvalidPersistedState),
        }
    }
    match (mcp, scheduler) {
        (Some(mcp), Some(scheduler)) => Ok((mcp, scheduler)),
        _ => Err(WorkloadError::InvalidPersistedState),
    }
}

fn parse_desired(value: &str) -> Result<DesiredState, WorkloadError> {
    match value {
        "running" => Ok(DesiredState::Running),
        "stopped" => Ok(DesiredState::Stopped),
        _ => Err(WorkloadError::InvalidPersistedState),
    }
}

fn initial_status(desired: DesiredState, running: bool) -> MutableStatus {
    MutableStatus {
        desired,
        actual: if running {
            ActualState::Running
        } else {
            ActualState::Stopped
        },
        last_error: None,
    }
}

fn selected_components(component: WorkloadComponent) -> &'static [WorkloadComponent] {
    match component {
        WorkloadComponent::Mcp => &[WorkloadComponent::Mcp],
        WorkloadComponent::Scheduler => &[WorkloadComponent::Scheduler],
        WorkloadComponent::All => &[WorkloadComponent::Mcp, WorkloadComponent::Scheduler],
    }
}

fn status_mut(state: &mut ControllerState, component: WorkloadComponent) -> &mut MutableStatus {
    match component {
        WorkloadComponent::Mcp => &mut state.mcp,
        WorkloadComponent::Scheduler => &mut state.scheduler,
        WorkloadComponent::All => unreachable!("virtual component has no state row"),
    }
}

fn snapshot(component: WorkloadComponent, status: &MutableStatus) -> WorkloadStatus {
    WorkloadStatus {
        component,
        desired: status.desired,
        actual: status.actual,
        last_error: status.last_error.clone(),
    }
}

fn bound_error(error: &str) -> String {
    if error.len() <= MAX_LAST_ERROR_BYTES {
        return error.to_owned();
    }
    let mut end = MAX_LAST_ERROR_BYTES;
    while !error.is_char_boundary(end) {
        end -= 1;
    }
    error[..end].to_owned()
}

fn refresh_runtime_state(status: &mut MutableStatus, running: bool) {
    if status.actual == ActualState::Running && !running {
        status.actual = ActualState::Failed;
        status.last_error = Some("workload exited unexpectedly".to_owned());
    }
}
