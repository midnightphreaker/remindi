use std::{collections::HashMap, fmt, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{
    Client, StatusCode, Url,
    header::{CONTENT_LENGTH, CONTENT_TYPE, LOCATION},
    redirect::Policy,
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::Deserialize;
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    AdapterConfigError, AdapterResult, AdapterStatus, ConditionAdapter, NetworkPolicy, guarded,
    invalid_parameters, resolve_target, valid_alias,
};
use crate::clock::Clock;

const MAX_REDIRECTS: usize = 3;
const MAX_CONFIGURED_RESPONSE_BYTES: usize = 1_048_576;

/// One administrator-configured HTTPS health target.
#[derive(Clone)]
pub struct HttpTarget {
    url: Url,
    expected_statuses: Vec<u16>,
    max_response_bytes: usize,
    expected_content_type: Option<String>,
    allow_redirects: bool,
    policy: NetworkPolicy,
}

impl HttpTarget {
    pub fn new(
        url: &str,
        expected_statuses: Vec<u16>,
        max_response_bytes: usize,
        expected_content_type: Option<String>,
        allow_redirects: bool,
        policy: NetworkPolicy,
    ) -> Result<Self, AdapterConfigError> {
        let url = Url::parse(url).map_err(|_| AdapterConfigError::Invalid)?;
        let content_type_valid = expected_content_type.as_ref().is_none_or(|content_type| {
            !content_type.is_empty()
                && content_type.len() <= 128
                && !content_type.chars().any(char::is_control)
        });
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.host_str().is_none()
            || url.fragment().is_some()
            || expected_statuses.is_empty()
            || expected_statuses
                .iter()
                .any(|status| !(100..=599).contains(status))
            || max_response_bytes == 0
            || max_response_bytes > MAX_CONFIGURED_RESPONSE_BYTES
            || !content_type_valid
        {
            return Err(AdapterConfigError::Invalid);
        }
        Ok(Self {
            url,
            expected_statuses,
            max_response_bytes,
            expected_content_type,
            allow_redirects,
            policy,
        })
    }
}

impl fmt::Debug for HttpTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpTarget")
            .field("origin", &"REDACTED")
            .field("expected_statuses", &self.expected_statuses)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("allow_redirects", &self.allow_redirects)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Parameters {
    target: String,
    expected_status: Option<u16>,
}

/// HTTPS-only, bounded health-check adapter.
pub struct HttpHealthAdapter {
    enabled: bool,
    aliases: HashMap<String, HttpTarget>,
    clock: Arc<dyn Clock>,
}

impl HttpHealthAdapter {
    #[must_use]
    pub fn disabled(clock: Arc<dyn Clock>) -> Self {
        Self {
            enabled: false,
            aliases: HashMap::new(),
            clock,
        }
    }

    pub fn new(
        enabled: bool,
        aliases: HashMap<String, HttpTarget>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, AdapterConfigError> {
        if aliases.keys().any(|alias| !valid_alias(alias)) {
            return Err(AdapterConfigError::Invalid);
        }
        Ok(Self {
            enabled,
            aliases,
            clock,
        })
    }
}

#[async_trait]
impl ConditionAdapter for HttpHealthAdapter {
    fn name(&self) -> &'static str {
        "http_health"
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
        if !valid_alias(&parameters.target) {
            return invalid_parameters(self.clock.as_ref(), started);
        }
        let Some(target) = self.aliases.get(&parameters.target).cloned() else {
            return AdapterResult::new(
                AdapterStatus::Unknown,
                self.clock.now(),
                "Configured target alias was not found.",
                started,
            );
        };
        if parameters
            .expected_status
            .is_some_and(|status| !target.expected_statuses.contains(&status))
        {
            return invalid_parameters(self.clock.as_ref(), started);
        }
        guarded(self.clock.as_ref(), deadline, cancel, async move {
            match probe(target, parameters.expected_status).await {
                Ok(true) => (
                    AdapterStatus::Satisfied,
                    "Configured target reported healthy.",
                ),
                Ok(false) => (
                    AdapterStatus::Unsatisfied,
                    "Configured target did not report healthy.",
                ),
                Err(_) => (
                    AdapterStatus::Error,
                    "Configured target health check failed safely.",
                ),
            }
        })
        .await
    }
}

async fn probe(
    target: HttpTarget,
    expected_status: Option<u16>,
) -> Result<bool, AdapterConfigError> {
    let original_origin = origin(&target.url)?;
    let mut current = target.url.clone();
    for redirect_count in 0..=MAX_REDIRECTS {
        if origin(&current)? != original_origin {
            return Err(AdapterConfigError::DestinationDenied);
        }
        let host = current.host_str().ok_or(AdapterConfigError::Invalid)?;
        let port = current
            .port_or_known_default()
            .ok_or(AdapterConfigError::Invalid)?;
        let addresses = resolve_target(host, port, target.policy).await?;
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(Policy::none())
            .resolve_to_addrs(host, &addresses)
            .build()
            .map_err(|_| AdapterConfigError::Invalid)?;
        let response = client
            .get(current.clone())
            .send()
            .await
            .map_err(|_| AdapterConfigError::Invalid)?;

        if response.status().is_redirection() {
            if !target.allow_redirects || redirect_count == MAX_REDIRECTS {
                return Err(AdapterConfigError::DestinationDenied);
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(AdapterConfigError::Invalid)?;
            current = next_redirect(&current, location)?;
            continue;
        }

        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > target.max_response_bytes)
        {
            return Err(AdapterConfigError::Invalid);
        }
        if target
            .expected_content_type
            .as_ref()
            .is_some_and(|expected| {
                response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_none_or(|actual| {
                        actual
                            .split(';')
                            .next()
                            .is_none_or(|actual| !actual.eq_ignore_ascii_case(expected))
                    })
            })
        {
            return Ok(false);
        }
        let status = response.status();
        consume_bounded(response, target.max_response_bytes).await?;
        let expected = expected_status
            .map(StatusCode::from_u16)
            .transpose()
            .map_err(|_| AdapterConfigError::Invalid)?;
        return Ok(expected.map_or_else(
            || target.expected_statuses.contains(&status.as_u16()),
            |expected| status == expected,
        ));
    }
    Err(AdapterConfigError::Invalid)
}

async fn consume_bounded(
    response: reqwest::Response,
    maximum: usize,
) -> Result<(), AdapterConfigError> {
    let mut total = 0_usize;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AdapterConfigError::Invalid)?;
        total = bounded_total(total, chunk.len(), maximum)?;
    }
    Ok(())
}

fn next_redirect(current: &Url, location: &str) -> Result<Url, AdapterConfigError> {
    let destination = current
        .join(location)
        .map_err(|_| AdapterConfigError::Invalid)?;
    if destination.scheme() != "https"
        || !destination.username().is_empty()
        || destination.password().is_some()
        || origin(&destination)? != origin(current)?
    {
        return Err(AdapterConfigError::DestinationDenied);
    }
    Ok(destination)
}

fn bounded_total(
    current: usize,
    additional: usize,
    maximum: usize,
) -> Result<usize, AdapterConfigError> {
    let total = current
        .checked_add(additional)
        .ok_or(AdapterConfigError::Invalid)?;
    if total > maximum {
        Err(AdapterConfigError::Invalid)
    } else {
        Ok(total)
    }
}

fn origin(url: &Url) -> Result<(&str, u16), AdapterConfigError> {
    Ok((
        url.host_str().ok_or(AdapterConfigError::Invalid)?,
        url.port_or_known_default()
            .ok_or(AdapterConfigError::Invalid)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{bounded_total, next_redirect};

    #[test]
    fn redirect_policy_allows_only_same_origin_https_destinations() {
        let current = reqwest::Url::parse("https://example.com/health").unwrap();

        assert!(next_redirect(&current, "/ready").is_ok());
        assert!(next_redirect(&current, "https://evil.example/ready").is_err());
        assert!(next_redirect(&current, "http://example.com/ready").is_err());
        assert!(next_redirect(&current, "https://user:secret@example.com/ready").is_err());
    }

    #[test]
    fn response_size_accounting_rejects_excess_and_overflow() {
        assert_eq!(bounded_total(5, 5, 10).unwrap(), 10);
        assert!(bounded_total(5, 6, 10).is_err());
        assert!(bounded_total(usize::MAX, 1, usize::MAX).is_err());
    }
}
