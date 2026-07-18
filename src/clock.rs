use time::OffsetDateTime;
use uuid::Uuid;

/// Supplies wall-clock time to domain and infrastructure code.
pub trait Clock: Send + Sync {
    /// Returns the current UTC instant.
    fn now(&self) -> OffsetDateTime;
}

/// Production clock backed by the operating system.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Deterministic clock for controlled application assembly.
#[derive(Clone, Copy, Debug)]
pub struct FixedClock {
    instant: OffsetDateTime,
}

impl FixedClock {
    /// Creates a clock fixed at `instant`.
    #[must_use]
    pub const fn new(instant: OffsetDateTime) -> Self {
        Self { instant }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.instant
    }
}

/// Supplies sortable identifiers without coupling callers to wall-clock time.
pub trait IdGenerator: Send + Sync {
    /// Produces the next UUID.
    fn next_id(&self) -> Uuid;
}

/// Production UUID version 7 generator.
#[derive(Clone, Copy, Debug, Default)]
pub struct UuidV7Generator;

impl IdGenerator for UuidV7Generator {
    fn next_id(&self) -> Uuid {
        Uuid::now_v7()
    }
}

/// Deterministic ID generator for controlled application assembly.
#[derive(Clone, Copy, Debug)]
pub struct FixedIdGenerator {
    id: Uuid,
}

impl FixedIdGenerator {
    /// Creates a generator that always returns `id`.
    #[must_use]
    pub const fn new(id: Uuid) -> Self {
        Self { id }
    }
}

impl IdGenerator for FixedIdGenerator {
    fn next_id(&self) -> Uuid {
        self.id
    }
}
