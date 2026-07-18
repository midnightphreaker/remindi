use std::time::Duration as StdDuration;

use serde_json::Value;
use time::{Duration, OffsetDateTime};

use super::{DomainError, EvidenceType};

/// Where a proposed evidence record originated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceSource {
    AuthenticatedActor,
    AdapterTrigger,
}

/// Completion evidence before application-boundary validation.
#[derive(Clone, Debug, PartialEq)]
pub struct EvidenceInput {
    pub evidence_type: EvidenceType,
    pub summary: String,
    pub reference_uri: Option<String>,
    pub content_hash: Option<String>,
    pub observed_at: OffsetDateTime,
    pub metadata: Option<Value>,
    pub source: EvidenceSource,
}

/// Structurally valid completion evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedEvidence(EvidenceInput);

impl ValidatedEvidence {
    /// Returns the validated concise observation summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.0.summary
    }

    /// Returns the normalized SHA-256 hash, when one was supplied.
    #[must_use]
    pub fn content_hash(&self) -> Option<&str> {
        self.0.content_hash.as_deref()
    }
}

impl EvidenceInput {
    /// Validates structure and provenance using a caller-supplied future-skew policy.
    pub fn validate(
        mut self,
        now: OffsetDateTime,
        maximum_future_skew: StdDuration,
    ) -> Result<ValidatedEvidence, DomainError> {
        self.summary = self.summary.trim().to_owned();
        let normalized_summary = self.summary.to_ascii_lowercase();
        if self.summary.is_empty()
            || self.summary.chars().count() > 4096
            || matches!(normalized_summary.as_str(), "done" | "looks good")
        {
            return Err(DomainError::EmptyEvidenceAssertion);
        }
        if self.source == EvidenceSource::AdapterTrigger {
            return Err(DomainError::AdapterResultIsNotCompletionEvidence);
        }
        if self.reference_uri.is_none() && self.content_hash.is_none() {
            return Err(DomainError::StableEvidenceReferenceRequired);
        }
        if let Some(reference) = self.reference_uri.as_deref() {
            validate_reference(reference)?;
        }
        if let Some(hash) = self.content_hash.as_mut() {
            *hash = normalize_hash(hash)?;
        }
        if self
            .metadata
            .as_ref()
            .is_some_and(|value| !value.is_object())
        {
            return Err(DomainError::EvidenceMetadataMustBeObject);
        }
        let future_skew = Duration::try_from(maximum_future_skew)
            .map_err(|_| DomainError::EvidenceObservedInFuture)?;
        if self.observed_at > now + future_skew {
            return Err(DomainError::EvidenceObservedInFuture);
        }
        Ok(ValidatedEvidence(self))
    }
}

fn validate_reference(reference: &str) -> Result<(), DomainError> {
    if reference.chars().count() > 4096 {
        return Err(DomainError::InvalidEvidenceReference);
    }
    if !valid_uri(reference) {
        return Err(DomainError::InvalidEvidenceReference);
    }
    if authority_contains_credentials(reference) {
        return Err(DomainError::EvidenceReferenceContainsCredentials);
    }
    Ok(())
}

fn valid_uri(reference: &str) -> bool {
    let Some((scheme, remainder)) = reference.split_once(':') else {
        return false;
    };
    let mut scheme_bytes = scheme.bytes();
    matches!(scheme_bytes.next(), Some(byte) if byte.is_ascii_alphabetic())
        && scheme_bytes
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
        && !remainder.is_empty()
        && !reference.chars().any(char::is_whitespace)
}

fn authority_contains_credentials(reference: &str) -> bool {
    let Some((_, remainder)) = reference.split_once("://") else {
        return false;
    };
    remainder
        .split(['/', '?', '#'])
        .next()
        .is_some_and(|authority| authority.contains('@'))
}

fn normalize_hash(hash: &str) -> Result<String, DomainError> {
    let value = hash.strip_prefix("sha256:").unwrap_or(hash);
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(DomainError::InvalidContentHash);
    }
    Ok(format!("sha256:{}", value.to_ascii_lowercase()))
}
