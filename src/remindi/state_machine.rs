use time::OffsetDateTime;

use super::{DomainError, EventType, Remindi, RemindiState, Trigger, ValidatedEvidence};

impl Remindi {
    /// Snoozes a currently ready occurrence without moving its schedule anchor.
    pub fn snooze(
        &mut self,
        snooze_until: OffsetDateTime,
        reason: &str,
        now: OffsetDateTime,
    ) -> Result<EventType, DomainError> {
        if self.state.is_terminal() {
            return Err(DomainError::TerminalState);
        }
        if !matches!(self.state, RemindiState::Due | RemindiState::Overdue) {
            return Err(DomainError::SnoozeRequiresReadyState);
        }
        if reason.trim().is_empty() {
            return Err(DomainError::ReasonRequired);
        }
        if snooze_until <= now {
            return Err(DomainError::SnoozeMustBeFuture);
        }

        self.snoozed_from_state = Some(self.state);
        self.snooze_until = Some(snooze_until);
        self.state = RemindiState::Snoozed;
        self.snooze_count += 1;
        self.version += 1;
        self.updated_at = now;
        Ok(EventType::Snoozed)
    }

    /// Completes any active item after evidence has passed structural validation.
    pub fn complete(
        &mut self,
        evidence: &ValidatedEvidence,
        now: OffsetDateTime,
    ) -> Result<EventType, DomainError> {
        if !self.state.is_active() {
            return Err(DomainError::TerminalState);
        }
        if evidence.summary().is_empty() {
            return Err(DomainError::CompletionEvidenceRequired);
        }

        self.clear_snooze();
        self.state = RemindiState::Completed;
        self.completed_at = Some(now);
        self.version += 1;
        self.updated_at = now;
        Ok(EventType::Completed)
    }

    /// Soft-cancels an active item while preserving its record and history.
    pub fn cancel(&mut self, reason: &str, now: OffsetDateTime) -> Result<EventType, DomainError> {
        if !self.state.is_active() {
            return Err(DomainError::TerminalState);
        }
        if reason.trim().is_empty() {
            return Err(DomainError::ReasonRequired);
        }

        self.clear_snooze();
        self.state = RemindiState::Cancelled;
        self.cancelled_at = Some(now);
        self.version += 1;
        self.updated_at = now;
        Ok(EventType::Cancelled)
    }

    /// Replaces an active trigger, resetting a snoozed item to a fresh schedule.
    pub fn replace_trigger(
        &mut self,
        trigger: Trigger,
        next_fire_at: Option<OffsetDateTime>,
        now: OffsetDateTime,
    ) -> Result<EventType, DomainError> {
        if !self.state.is_active() {
            return Err(DomainError::TerminalState);
        }
        trigger.validate()?;

        self.trigger = trigger;
        self.next_fire_at = next_fire_at;
        self.original_next_fire_at = next_fire_at;
        self.next_evaluation_at = None;
        if self.state == RemindiState::Snoozed {
            self.due_since = None;
            self.clear_snooze();
            self.state = RemindiState::Scheduled;
        }
        self.version += 1;
        self.updated_at = now;
        Ok(EventType::Updated)
    }

    pub(crate) fn clear_snooze(&mut self) {
        self.snooze_until = None;
        self.snoozed_from_state = None;
    }
}
