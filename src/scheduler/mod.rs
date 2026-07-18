//! Single-process background readiness evaluation.

mod lease;
mod runner;

pub use lease::{LeaseError, LeaseGuard, SchedulerLease};
pub use runner::{
    AdapterProvider, PollReport, RunExit, Scheduler, SchedulerConfig, SchedulerError,
};
