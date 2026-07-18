use time::{Duration, OffsetDateTime};

use super::{DomainError, MissedPolicy, OccurrenceDisposition, RecurrenceSpec, Trigger};

/// The next anchored occurrence after an explicit disposition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecurrenceAdvance {
    pub next_fire_at: OffsetDateTime,
    pub occurrence_no: u64,
    pub skipped_count: u64,
}

impl RecurrenceSpec {
    /// Validates the version-1 fixed recurrence bounds.
    pub fn validate(&self) -> Result<(), DomainError> {
        if !(60..=31_536_000).contains(&self.every_seconds) {
            return Err(DomainError::InvalidRecurrenceInterval);
        }
        if self
            .max_occurrences
            .is_some_and(|maximum| !(1..=1_000_000).contains(&maximum))
        {
            return Err(DomainError::InvalidMaxOccurrences);
        }
        Ok(())
    }

    /// Validates that recurrence is paired with an equal fixed interval trigger.
    pub fn validate_for_trigger(&self, trigger: &Trigger) -> Result<(), DomainError> {
        self.validate()?;
        let Trigger::Interval { every_seconds, .. } = trigger else {
            return Err(DomainError::RecurrenceRequiresIntervalTrigger);
        };
        if *every_seconds != self.every_seconds {
            return Err(DomainError::RecurrenceIntervalMismatch);
        }
        Ok(())
    }

    /// Calculates the next occurrence from the scheduled anchor, never from `now`.
    pub fn advance(
        &self,
        previous_next_fire_at: OffsetDateTime,
        occurrence_no: u64,
        now: OffsetDateTime,
        _disposition: OccurrenceDisposition,
    ) -> Result<RecurrenceAdvance, DomainError> {
        self.validate()?;
        let interval = Duration::seconds(
            i64::try_from(self.every_seconds)
                .map_err(|_| DomainError::InvalidRecurrenceInterval)?,
        );
        let mut next = previous_next_fire_at + interval;
        let mut next_occurrence = occurrence_no + 1;
        let mut skipped_count = 0;

        self.ensure_permitted(next, next_occurrence)?;

        if matches!(
            self.missed_policy,
            MissedPolicy::Coalesce | MissedPolicy::Skip
        ) {
            while next <= now {
                if self.missed_policy == MissedPolicy::Skip {
                    skipped_count += 1;
                }
                next += interval;
                next_occurrence += 1;
                self.ensure_permitted(next, next_occurrence)?;
            }
        }

        Ok(RecurrenceAdvance {
            next_fire_at: next,
            occurrence_no: next_occurrence,
            skipped_count,
        })
    }

    /// Counts ready catch-up occurrences without advancing stored state.
    pub fn ready_occurrences(
        &self,
        current_anchor: OffsetDateTime,
        occurrence_no: u64,
        now: OffsetDateTime,
        maximum: usize,
    ) -> Result<usize, DomainError> {
        self.validate()?;
        if maximum == 0 {
            return Err(DomainError::InvalidCatchUpLimit);
        }
        let interval = Duration::seconds(
            i64::try_from(self.every_seconds)
                .map_err(|_| DomainError::InvalidRecurrenceInterval)?,
        );
        let mut anchor = current_anchor;
        let mut number = occurrence_no;
        let mut ready = 0;

        while anchor <= now && ready < maximum {
            if self.ensure_permitted(anchor, number).is_err() {
                break;
            }
            ready += 1;
            anchor += interval;
            number += 1;
        }
        Ok(ready)
    }

    fn ensure_permitted(
        &self,
        next_fire_at: OffsetDateTime,
        occurrence_no: u64,
    ) -> Result<(), DomainError> {
        if self
            .max_occurrences
            .is_some_and(|maximum| occurrence_no > maximum)
            || self.end_at.is_some_and(|end_at| next_fire_at > end_at)
        {
            return Err(DomainError::FinalOccurrence);
        }
        Ok(())
    }
}
