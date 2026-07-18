use http::{HeaderMap, Method};
use subtle::ConstantTimeEq;

/// Header carrying the session-bound browser mutation token.
pub const CSRF_HEADER: &str = "x-csrf-token";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsrfError {
    MissingOrigin,
    CrossOrigin,
    MissingToken,
    InvalidToken,
}

/// Validates an exact HTTP(S) same-origin request against the request Host.
pub fn validate_same_origin(headers: &HeaderMap) -> Result<(), CsrfError> {
    if headers.get_all(http::header::HOST).iter().count() != 1
        || headers.get_all(http::header::ORIGIN).iter().count() != 1
    {
        return Err(CsrfError::MissingOrigin);
    }
    let host = headers
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(CsrfError::MissingOrigin)?;
    let origin = headers
        .get(http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(CsrfError::MissingOrigin)?;
    let authority = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .filter(|value| !value.contains('/') && !value.contains('@'))
        .ok_or(CsrfError::CrossOrigin)?;
    if authority.eq_ignore_ascii_case(host) {
        Ok(())
    } else {
        Err(CsrfError::CrossOrigin)
    }
}

/// Validates the session-bound token for every state-changing HTTP method.
pub fn validate_mutation(
    method: &Method,
    headers: &HeaderMap,
    expected_token: &str,
) -> Result<(), CsrfError> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    validate_same_origin(headers)?;
    if headers.get_all(CSRF_HEADER).iter().count() != 1 {
        return Err(CsrfError::MissingToken);
    }
    let supplied = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(CsrfError::MissingToken)?;
    bool::from(supplied.as_bytes().ct_eq(expected_token.as_bytes()))
        .then_some(())
        .ok_or(CsrfError::InvalidToken)
}

#[cfg(test)]
mod tests {
    use http::{HeaderMap, HeaderValue, Method, header};

    use super::{CsrfError, validate_mutation, validate_same_origin};

    fn headers(origin: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("remindi.local:8000"));
        headers.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
        headers
    }

    #[test]
    fn origin_requires_exact_authority() {
        assert_eq!(
            validate_same_origin(&headers("https://evil.example")),
            Err(CsrfError::CrossOrigin)
        );
        assert!(validate_same_origin(&headers("https://remindi.local:8000")).is_ok());
    }

    #[test]
    fn mutation_requires_bound_token() {
        let mut headers = headers("https://remindi.local:8000");
        headers.insert("x-csrf-token", HeaderValue::from_static("right"));
        assert!(validate_mutation(&Method::POST, &headers, "right").is_ok());
        assert_eq!(
            validate_mutation(&Method::POST, &headers, "wrong"),
            Err(CsrfError::InvalidToken)
        );
        headers.append("x-csrf-token", HeaderValue::from_static("right"));
        assert_eq!(
            validate_mutation(&Method::POST, &headers, "right"),
            Err(CsrfError::MissingToken)
        );
    }
}
