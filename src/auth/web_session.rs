use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use http::{HeaderMap, HeaderValue, header};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::{Duration, OffsetDateTime};

use crate::config::BootstrapConfig;

pub const SESSION_COOKIE: &str = "remindi_session";
const MAX_LOGIN_ATTEMPTS: usize = 5;
const LOGIN_WINDOW: Duration = Duration::minutes(1);
const MAX_TRACKED_ATTEMPTS: usize = 1_024;
const MAX_SESSIONS: usize = 1_024;
const MAX_PRE_SESSION_TOKENS: usize = 1_024;
const PRE_SESSION_TTL: Duration = Duration::minutes(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebMode {
    Disabled,
    Authenticated,
    Unauthenticated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionView {
    pub actor_id: String,
    pub csrf_token: String,
    pub expires_at: OffsetDateTime,
    pub reauthenticated_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginError {
    Disabled,
    CsrfRejected,
    RateLimited,
    InvalidCredentials,
    Randomness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    Disabled,
    Unauthenticated,
    Expired,
}

#[derive(Clone)]
pub struct WebSessionManager {
    inner: Arc<Inner>,
}

struct Inner {
    mode: WebMode,
    username_digest: Option<[u8; 32]>,
    password_digest: Option<[u8; 32]>,
    ttl: Duration,
    cookie_secure: bool,
    unauthenticated_csrf: String,
    sessions: Mutex<HashMap<String, StoredSession>>,
    pre_session: Mutex<HashMap<String, OffsetDateTime>>,
    login_attempts: Mutex<VecDeque<OffsetDateTime>>,
}

#[derive(Clone)]
struct StoredSession {
    actor_id: String,
    csrf_token: String,
    expires_at: OffsetDateTime,
    reauthenticated_at: OffsetDateTime,
}

pub struct LoginSuccess {
    pub session: SessionView,
    pub set_cookie: HeaderValue,
}

impl WebSessionManager {
    pub fn from_config(config: &BootstrapConfig) -> Result<Self, LoginError> {
        let mode = if !config.webui_enabled() {
            WebMode::Disabled
        } else if config.webui_auth_enabled() {
            WebMode::Authenticated
        } else {
            WebMode::Unauthenticated
        };
        let unauthenticated_csrf = random_token().map_err(|()| LoginError::Randomness)?;
        Ok(Self {
            inner: Arc::new(Inner {
                mode,
                username_digest: config.webui_username().map(secret_digest),
                password_digest: config.webui_password().map(secret_digest),
                ttl: Duration::seconds(
                    i64::try_from(config.webui_session_ttl_seconds()).unwrap_or(i64::MAX),
                ),
                cookie_secure: config.webui_cookie_secure(),
                unauthenticated_csrf,
                sessions: Mutex::new(HashMap::new()),
                pre_session: Mutex::new(HashMap::new()),
                login_attempts: Mutex::new(VecDeque::new()),
            }),
        })
    }

    #[must_use]
    pub fn mode(&self) -> WebMode {
        self.inner.mode
    }

    pub fn issue_pre_session_token(&self, now: OffsetDateTime) -> Result<String, LoginError> {
        if self.inner.mode == WebMode::Disabled {
            return Err(LoginError::Disabled);
        }
        let token = random_token().map_err(|()| LoginError::Randomness)?;
        let mut nonces = lock(&self.inner.pre_session);
        nonces.retain(|_, expires| *expires > now);
        evict_one_at_capacity(&mut nonces, MAX_PRE_SESSION_TOKENS);
        nonces.insert(token.clone(), now + PRE_SESSION_TTL);
        Ok(token)
    }

    pub fn login(
        &self,
        username: &str,
        password: &str,
        pre_session_token: &str,
        now: OffsetDateTime,
    ) -> Result<LoginSuccess, LoginError> {
        if self.inner.mode != WebMode::Authenticated {
            return Err(LoginError::Disabled);
        }
        self.consume_pre_session(pre_session_token, now)?;
        self.check_rate_limit(now)?;
        let username_valid = self
            .inner
            .username_digest
            .is_some_and(|expected| digest(username).ct_eq(&expected).into());
        let password_valid = self
            .inner
            .password_digest
            .is_some_and(|expected| digest(password).ct_eq(&expected).into());
        if !(username_valid & password_valid) {
            return Err(LoginError::InvalidCredentials);
        }

        let id = random_token().map_err(|()| LoginError::Randomness)?;
        let csrf_token = random_token().map_err(|()| LoginError::Randomness)?;
        let actor_id = actor_id(username);
        let stored = StoredSession {
            actor_id: actor_id.clone(),
            csrf_token: csrf_token.clone(),
            expires_at: now + self.inner.ttl,
            reauthenticated_at: now,
        };
        let mut sessions = lock(&self.inner.sessions);
        sessions.retain(|_, session| session.expires_at > now);
        evict_one_at_capacity(&mut sessions, MAX_SESSIONS);
        sessions.insert(id.clone(), stored.clone());
        let secure = if self.inner.cookie_secure {
            "; Secure"
        } else {
            ""
        };
        let cookie = format!(
            "{SESSION_COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
            self.inner.ttl.whole_seconds(),
            secure
        );
        let set_cookie = HeaderValue::from_str(&cookie).map_err(|_| LoginError::Randomness)?;
        Ok(LoginSuccess {
            session: view(stored),
            set_cookie,
        })
    }

    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        now: OffsetDateTime,
    ) -> Result<SessionView, SessionError> {
        match self.inner.mode {
            WebMode::Disabled => Err(SessionError::Disabled),
            WebMode::Unauthenticated => Ok(SessionView {
                actor_id: "webui:unauthenticated".to_owned(),
                csrf_token: self.inner.unauthenticated_csrf.clone(),
                expires_at: OffsetDateTime::UNIX_EPOCH + Duration::days(365_000),
                reauthenticated_at: now,
            }),
            WebMode::Authenticated => {
                let id =
                    cookie_value(headers, SESSION_COOKIE).ok_or(SessionError::Unauthenticated)?;
                let mut sessions = lock(&self.inner.sessions);
                let session = sessions
                    .get(id)
                    .cloned()
                    .ok_or(SessionError::Unauthenticated)?;
                if session.expires_at <= now {
                    sessions.remove(id);
                    return Err(SessionError::Expired);
                }
                Ok(view(session))
            }
        }
    }

    pub fn logout(&self, headers: &HeaderMap) -> HeaderValue {
        if let Some(id) = cookie_value(headers, SESSION_COOKIE) {
            lock(&self.inner.sessions).remove(id);
        }
        if self.inner.cookie_secure {
            HeaderValue::from_static(
                "remindi_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0; Secure",
            )
        } else {
            HeaderValue::from_static(
                "remindi_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
            )
        }
    }

    fn consume_pre_session(&self, token: &str, now: OffsetDateTime) -> Result<(), LoginError> {
        let mut nonces = lock(&self.inner.pre_session);
        let expires = nonces.remove(token).ok_or(LoginError::CsrfRejected)?;
        (expires > now)
            .then_some(())
            .ok_or(LoginError::CsrfRejected)
    }

    fn check_rate_limit(&self, now: OffsetDateTime) -> Result<(), LoginError> {
        let mut attempts = lock(&self.inner.login_attempts);
        while attempts.front().is_some_and(|at| *at <= now - LOGIN_WINDOW) {
            attempts.pop_front();
        }
        if attempts.len() >= MAX_LOGIN_ATTEMPTS {
            return Err(LoginError::RateLimited);
        }
        attempts.push_back(now);
        while attempts.len() > MAX_TRACKED_ATTEMPTS {
            attempts.pop_front();
        }
        Ok(())
    }
}

fn secret_digest(secret: &SecretString) -> [u8; 32] {
    digest(secret.expose_secret())
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn actor_id(username: &str) -> String {
    format!("webui:{}", URL_SAFE_NO_PAD.encode(digest(username)))
}

fn random_token() -> Result<String, ()> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| ())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    if headers.get_all(header::COOKIE).iter().count() != 1 {
        return None;
    }
    let mut values = headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .filter_map(|(key, value)| (key == name).then_some(value));
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

fn view(session: StoredSession) -> SessionView {
    SessionView {
        actor_id: session.actor_id,
        csrf_token: session.csrf_token,
        expires_at: session.expires_at,
        reauthenticated_at: session.reauthenticated_at,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn evict_one_at_capacity<T>(values: &mut HashMap<String, T>, capacity: usize) {
    if values.len() >= capacity
        && let Some(key) = values.keys().next().cloned()
    {
        values.remove(&key);
    }
}
