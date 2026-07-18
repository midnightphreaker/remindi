use std::str::FromStr;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http::{
    HeaderMap, HeaderValue, Uri,
    header::{AUTHORIZATION, COOKIE, HOST, ORIGIN},
    uri::Authority,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

const BEARER_PREFIX: &str = "Bearer ";
const TOKEN_CHECK_DOMAIN: &[u8] = b"remindi:mcp-bearer-check:v1\0";
const ACTOR_DOMAIN: &[u8] = b"remindi:mcp-actor:v1\0";

/// Authenticated identity established at the MCP HTTP boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpIdentity {
    owner_id: String,
    actor_id: String,
}

impl McpIdentity {
    /// Returns the fixed owner configured for this process.
    #[must_use]
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// Returns the stable, non-secret actor pseudonym for audit and idempotency.
    #[must_use]
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }
}

/// Safe MCP request-policy failures that never expose credential values.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum McpAuthError {
    /// The configured Host policy contains an invalid authority.
    #[error("MCP Host policy is invalid")]
    InvalidHostPolicy,
    /// The configured Origin policy contains an invalid origin.
    #[error("MCP Origin policy is invalid")]
    InvalidOriginPolicy,
    /// Authentication was absent, malformed, duplicated, or incorrect.
    #[error("MCP authentication failed")]
    Unauthenticated,
    /// A request attempted to carry credentials outside Authorization.
    #[error("MCP credentials must use the Authorization header")]
    CredentialLocation,
    /// The request Host was absent, malformed, duplicated, or disallowed.
    #[error("MCP Host was rejected")]
    HostRejected,
    /// The request Origin was malformed, duplicated, or disallowed.
    #[error("MCP Origin was rejected")]
    OriginRejected,
}

/// Immutable bearer, Host, and Origin policy for the `/mcp` route.
///
/// The raw bearer token is reduced to domain-separated digests during
/// construction and is not retained by this type.
pub struct McpAuthPolicy {
    owner_id: String,
    expected_token_digest: [u8; 32],
    actor_id: String,
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
}

impl McpAuthPolicy {
    /// Builds an immutable policy from the process bootstrap values.
    ///
    /// # Errors
    ///
    /// Returns a safe policy error when a configured Host or Origin is not a
    /// normalized HTTP authority or origin.
    pub fn new<'a, Hosts, Origins>(
        owner_id: &str,
        token: &SecretString,
        allowed_hosts: Hosts,
        allowed_origins: Origins,
    ) -> Result<Self, McpAuthError>
    where
        Hosts: IntoIterator<Item = &'a str>,
        Origins: IntoIterator<Item = &'a str>,
    {
        let token = token.expose_secret().as_bytes();
        let allowed_hosts = allowed_hosts
            .into_iter()
            .map(normalize_authority)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|()| McpAuthError::InvalidHostPolicy)?;
        let allowed_origins = allowed_origins
            .into_iter()
            .map(normalize_origin)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|()| McpAuthError::InvalidOriginPolicy)?;

        Ok(Self {
            owner_id: owner_id.to_owned(),
            expected_token_digest: token_digest(token),
            actor_id: actor_fingerprint(owner_id.as_bytes(), token),
            allowed_hosts,
            allowed_origins,
        })
    }

    /// Authenticates one `/mcp` request and enforces its transport policy.
    ///
    /// # Errors
    ///
    /// Returns a minimally revealing error for invalid credentials, credential
    /// placement, Host, or Origin.
    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        uri: &Uri,
    ) -> Result<McpIdentity, McpAuthError> {
        if headers.contains_key(COOKIE) || uri.query().is_some() {
            return Err(McpAuthError::CredentialLocation);
        }

        let host = exact_header(headers, HOST)
            .and_then(normalize_header_authority)
            .ok_or(McpAuthError::HostRejected)?;
        if !self.allowed_hosts.is_empty() && !self.allowed_hosts.contains(&host) {
            return Err(McpAuthError::HostRejected);
        }

        if let Some(origin_header) =
            optional_exact_header(headers, ORIGIN).map_err(|()| McpAuthError::OriginRejected)?
        {
            let origin =
                normalize_header_origin(origin_header).ok_or(McpAuthError::OriginRejected)?;
            let allowed = if self.allowed_origins.is_empty() {
                origin_authority(&origin) == host
            } else {
                self.allowed_origins.contains(&origin)
            };
            if !allowed {
                return Err(McpAuthError::OriginRejected);
            }
        }

        let authorization =
            exact_header(headers, AUTHORIZATION).ok_or(McpAuthError::Unauthenticated)?;
        let candidate = parse_bearer(authorization).ok_or(McpAuthError::Unauthenticated)?;
        let candidate_digest = token_digest(candidate);
        if !bool::from(candidate_digest.ct_eq(&self.expected_token_digest)) {
            return Err(McpAuthError::Unauthenticated);
        }

        Ok(McpIdentity {
            owner_id: self.owner_id.clone(),
            actor_id: self.actor_id.clone(),
        })
    }
}

fn exact_header(headers: &HeaderMap, name: http::header::HeaderName) -> Option<&HeaderValue> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn optional_exact_header(
    headers: &HeaderMap,
    name: http::header::HeaderName,
) -> Result<Option<&HeaderValue>, ()> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(());
    }
    Ok(first)
}

fn parse_bearer(header: &HeaderValue) -> Option<&[u8]> {
    let value = header.to_str().ok()?;
    let candidate = value.strip_prefix(BEARER_PREFIX)?;
    (!candidate.is_empty() && !candidate.bytes().any(|byte| byte.is_ascii_whitespace()))
        .then_some(candidate.as_bytes())
}

fn normalize_header_authority(header: &HeaderValue) -> Option<String> {
    let value = header.to_str().ok()?;
    normalize_authority(value).ok()
}

fn normalize_authority(value: &str) -> Result<String, ()> {
    if value.is_empty() || value.trim() != value || value.contains('@') {
        return Err(());
    }
    let authority = Authority::from_str(value).map_err(|_| ())?;
    let host = authority.host().to_ascii_lowercase();
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    Ok(match authority.port_u16() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn normalize_header_origin(header: &HeaderValue) -> Option<String> {
    let value = header.to_str().ok()?;
    normalize_origin(value).ok()
}

fn normalize_origin(value: &str) -> Result<String, ()> {
    if value.is_empty() || value.trim() != value {
        return Err(());
    }
    let (_, authority_text) = value.split_once("://").ok_or(())?;
    if authority_text.is_empty()
        || authority_text
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'?' | b'#'))
    {
        return Err(());
    }
    let uri = Uri::from_str(value).map_err(|_| ())?;
    let scheme = uri.scheme_str().ok_or(())?.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err(());
    }
    let authority = uri.authority().ok_or(())?;
    let authority = normalize_authority(authority.as_str())?;
    Ok(format!("{scheme}://{authority}"))
}

fn origin_authority(origin: &str) -> &str {
    origin
        .split_once("://")
        .map_or("", |(_, authority)| authority)
}

fn token_digest(token: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(TOKEN_CHECK_DOMAIN);
    digest.update(token);
    digest.finalize().into()
}

fn actor_fingerprint(owner_id: &[u8], token: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(ACTOR_DOMAIN);
    digest.update((owner_id.len() as u64).to_be_bytes());
    digest.update(owner_id);
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token);
    format!("mcp:{}", URL_SAFE_NO_PAD.encode(digest.finalize()))
}

#[cfg(test)]
mod tests {
    use http::{
        HeaderMap, HeaderValue, Uri,
        header::{AUTHORIZATION, COOKIE, HOST, ORIGIN},
    };
    use secrecy::SecretString;

    use super::{McpAuthError, McpAuthPolicy};

    const OWNER: &str = "owner-a";
    const TOKEN: &str = "test-mcp-token";

    fn policy(hosts: &[&str], origins: &[&str]) -> McpAuthPolicy {
        McpAuthPolicy::new(
            OWNER,
            &SecretString::from(TOKEN),
            hosts.iter().copied(),
            origins.iter().copied(),
        )
        .expect("valid policy")
    }

    fn valid_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("remindi.local:8000"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer test-mcp-token"),
        );
        headers
    }

    fn authenticate(policy: &McpAuthPolicy, headers: &HeaderMap) -> Result<String, McpAuthError> {
        policy
            .authenticate(headers, &Uri::from_static("/mcp"))
            .map(|identity| identity.actor_id().to_owned())
    }

    #[test]
    fn valid_bearer_authenticates_owner_and_returns_stable_actor() {
        let policy = policy(&[], &[]);
        let first = policy
            .authenticate(&valid_headers(), &Uri::from_static("/mcp"))
            .expect("valid credentials");
        let second = policy
            .authenticate(&valid_headers(), &Uri::from_static("/mcp"))
            .expect("valid credentials");

        assert_eq!(first.owner_id(), OWNER);
        assert_eq!(first.actor_id(), second.actor_id());
        assert!(first.actor_id().starts_with("mcp:"));
        assert!(!first.actor_id().contains(TOKEN));
        assert!(!first.actor_id().contains(OWNER));
    }

    #[test]
    fn actor_fingerprint_is_scoped_to_owner_and_token() {
        let owner_b = McpAuthPolicy::new(
            "owner-b",
            &SecretString::from(TOKEN),
            std::iter::empty::<&str>(),
            std::iter::empty::<&str>(),
        )
        .expect("valid policy");
        let token_b = McpAuthPolicy::new(
            OWNER,
            &SecretString::from("another-token"),
            std::iter::empty::<&str>(),
            std::iter::empty::<&str>(),
        )
        .expect("valid policy");

        let actor = authenticate(&policy(&[], &[]), &valid_headers()).expect("valid auth");
        let owner_actor = authenticate(&owner_b, &valid_headers()).expect("valid auth");
        let mut token_b_headers = valid_headers();
        token_b_headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer another-token"),
        );
        let token_actor = authenticate(&token_b, &token_b_headers).expect("valid auth");

        assert_ne!(actor, owner_actor);
        assert_ne!(actor, token_actor);
    }

    #[test]
    fn missing_duplicate_or_malformed_authorization_is_rejected() {
        let policy = policy(&[], &[]);
        let mut missing = valid_headers();
        missing.remove(AUTHORIZATION);
        assert_eq!(
            authenticate(&policy, &missing),
            Err(McpAuthError::Unauthenticated)
        );

        for malformed in [
            "bearer test-mcp-token",
            "Basic test-mcp-token",
            "Bearer",
            "Bearer  test-mcp-token",
            "Bearer test-mcp-token ",
        ] {
            let mut headers = valid_headers();
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(malformed).expect("valid header bytes"),
            );
            assert_eq!(
                authenticate(&policy, &headers),
                Err(McpAuthError::Unauthenticated)
            );
        }

        let mut duplicate = valid_headers();
        duplicate.append(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer test-mcp-token"),
        );
        assert_eq!(
            authenticate(&policy, &duplicate),
            Err(McpAuthError::Unauthenticated)
        );
    }

    #[test]
    fn incorrect_bearer_is_rejected_without_authentication_details() {
        let policy = policy(&[], &[]);
        let mut headers = valid_headers();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token"),
        );

        let error = policy
            .authenticate(&headers, &Uri::from_static("/mcp"))
            .expect_err("wrong token must fail");

        assert_eq!(error, McpAuthError::Unauthenticated);
        assert_eq!(error.to_string(), "MCP authentication failed");
        assert!(!error.to_string().contains("wrong-token"));
    }

    #[test]
    fn cookie_and_query_credentials_are_rejected() {
        let policy = policy(&[], &[]);
        let mut cookie = valid_headers();
        cookie.insert(COOKIE, HeaderValue::from_static("token=test-mcp-token"));
        assert_eq!(
            authenticate(&policy, &cookie),
            Err(McpAuthError::CredentialLocation)
        );

        for uri in [
            "/mcp?token=test-mcp-token",
            "/mcp?access_token=test-mcp-token",
            "/mcp?authorization=Bearer%20test-mcp-token",
        ] {
            assert_eq!(
                policy.authenticate(&valid_headers(), &uri.parse().expect("valid URI")),
                Err(McpAuthError::CredentialLocation)
            );
        }
    }

    #[test]
    fn configured_host_matches_case_insensitively_with_exact_port_semantics() {
        let policy = policy(&["REMINDI.LOCAL:8000"], &[]);
        assert!(authenticate(&policy, &valid_headers()).is_ok());

        let mut wrong_port = valid_headers();
        wrong_port.insert(HOST, HeaderValue::from_static("remindi.local:9000"));
        assert_eq!(
            authenticate(&policy, &wrong_port),
            Err(McpAuthError::HostRejected)
        );
    }

    #[test]
    fn missing_malformed_or_unlisted_host_is_rejected() {
        let policy = policy(&["remindi.local:8000"], &[]);
        let mut missing = valid_headers();
        missing.remove(HOST);
        assert_eq!(
            authenticate(&policy, &missing),
            Err(McpAuthError::HostRejected)
        );

        let mut malformed = valid_headers();
        malformed.insert(HOST, HeaderValue::from_static("https://remindi.local"));
        assert_eq!(
            authenticate(&policy, &malformed),
            Err(McpAuthError::HostRejected)
        );

        let mut unlisted = valid_headers();
        unlisted.insert(HOST, HeaderValue::from_static("elsewhere.local:8000"));
        assert_eq!(
            authenticate(&policy, &unlisted),
            Err(McpAuthError::HostRejected)
        );
    }

    #[test]
    fn absent_origin_is_allowed_for_non_browser_clients() {
        assert!(authenticate(&policy(&[], &[]), &valid_headers()).is_ok());
    }

    #[test]
    fn configured_origin_requires_an_exact_normalized_match() {
        let policy = policy(&[], &["HTTPS://REMINDI.LOCAL:8443"]);
        let mut headers = valid_headers();
        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://remindi.local:8443"),
        );
        assert!(authenticate(&policy, &headers).is_ok());

        headers.insert(
            ORIGIN,
            HeaderValue::from_static("https://remindi.local:9443"),
        );
        assert_eq!(
            authenticate(&policy, &headers),
            Err(McpAuthError::OriginRejected)
        );
    }

    #[test]
    fn empty_origin_allowlist_enforces_same_host_for_present_origin() {
        let policy = policy(&[], &[]);
        let mut same_host = valid_headers();
        same_host.insert(
            ORIGIN,
            HeaderValue::from_static("https://remindi.local:8000"),
        );
        assert!(authenticate(&policy, &same_host).is_ok());

        same_host.insert(
            ORIGIN,
            HeaderValue::from_static("https://elsewhere.local:8000"),
        );
        assert_eq!(
            authenticate(&policy, &same_host),
            Err(McpAuthError::OriginRejected)
        );
    }

    #[test]
    fn malformed_null_or_duplicate_origin_is_rejected() {
        let policy = policy(&[], &[]);
        for origin in ["null", "remindi.local:8000", "https://remindi.local/path"] {
            let mut headers = valid_headers();
            headers.insert(
                ORIGIN,
                HeaderValue::from_str(origin).expect("valid header bytes"),
            );
            assert_eq!(
                authenticate(&policy, &headers),
                Err(McpAuthError::OriginRejected)
            );
        }

        let mut duplicate = valid_headers();
        duplicate.append(
            ORIGIN,
            HeaderValue::from_static("https://remindi.local:8000"),
        );
        duplicate.append(
            ORIGIN,
            HeaderValue::from_static("https://remindi.local:8000"),
        );
        assert_eq!(
            authenticate(&policy, &duplicate),
            Err(McpAuthError::OriginRejected)
        );
    }
}
