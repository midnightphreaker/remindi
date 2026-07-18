use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

use secrecy::SecretString;
use thiserror::Error;

/// The only listener address used inside the production container.
pub const LISTEN_ADDRESS: &str = "0.0.0.0:8000";

const DEFAULT_DATABASE_PATH: &str = "/data/remindi.db";
const DEFAULT_BACKUP_DIRECTORY: &str = "/data/backups";
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_WEBUI_TITLE: &str = "Remindi";
const DEFAULT_SESSION_TTL_SECONDS: u64 = 43_200;

/// Fail-closed bootstrap configuration errors that never contain input values.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A required variable was absent or blank.
    #[error("{variable} is required{condition}")]
    Required {
        /// Environment variable name.
        variable: &'static str,
        /// Safe condition explaining when the variable is required.
        condition: &'static str,
    },
    /// A boolean variable was not exactly `true` or `false`.
    #[error("{variable} must be `true` or `false`")]
    InvalidBoolean {
        /// Environment variable name.
        variable: &'static str,
    },
    /// A numeric variable was malformed or outside its valid range.
    #[error("{variable} must be a positive integer")]
    InvalidPositiveInteger {
        /// Environment variable name.
        variable: &'static str,
    },
    /// A configured filesystem path was not absolute.
    #[error("{variable} must be an absolute path")]
    PathNotAbsolute {
        /// Environment variable name.
        variable: &'static str,
    },
    /// A relevant environment value was not valid Unicode.
    #[error("{variable} must contain valid Unicode")]
    NonUnicode {
        /// Environment variable name.
        variable: String,
    },
}

/// Environment-only process configuration.
///
/// Secret-bearing fields intentionally use [`SecretString`]. This type
/// intentionally does not implement `Debug` or `Serialize`.
pub struct BootstrapConfig {
    database_path: PathBuf,
    owner_id: String,
    mcp_token: SecretString,
    backup_directory: PathBuf,
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
    log_level: String,
    log_content: bool,
    webui_enabled: bool,
    webui_auth_enabled: bool,
    webui_username: Option<SecretString>,
    webui_password: Option<SecretString>,
    webui_session_ttl_seconds: u64,
    webui_cookie_secure: bool,
    webui_title: String,
    webui_custom_css_file: Option<PathBuf>,
    webui_logo_file: Option<PathBuf>,
    webui_favicon_file: Option<PathBuf>,
}

impl BootstrapConfig {
    /// Reads relevant `REMINDI_*` variables once from the process environment.
    ///
    /// # Errors
    ///
    /// Returns a redacted [`ConfigError`] when a relevant value is malformed
    /// or the bootstrap contract is incomplete.
    pub fn from_env() -> Result<Self, ConfigError> {
        let pairs = env::vars_os()
            .filter_map(relevant_environment_pair)
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_pairs(pairs)
    }

    /// Parses bootstrap configuration from key-value pairs.
    ///
    /// This boundary makes configuration deterministic without mutating the
    /// process environment in tests.
    ///
    /// # Errors
    ///
    /// Returns a redacted [`ConfigError`] when a value is malformed or a
    /// required setting is absent.
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let values = pairs
            .into_iter()
            .map(|(key, value)| (key.as_ref().to_owned(), value.as_ref().to_owned()))
            .collect::<HashMap<_, _>>();

        let database_path = absolute_path(
            "REMINDI_DB_PATH",
            value_or_default(&values, "REMINDI_DB_PATH", DEFAULT_DATABASE_PATH),
        )?;
        let owner_id = required(&values, "REMINDI_OWNER_ID", "")?;
        let mcp_token = required(&values, "REMINDI_MCP_TOKEN", "")?;
        let backup_directory = absolute_path(
            "REMINDI_BACKUP_DIR",
            value_or_default(&values, "REMINDI_BACKUP_DIR", DEFAULT_BACKUP_DIRECTORY),
        )?;
        let log_content = boolean(&values, "REMINDI_LOG_CONTENT", false)?;
        let webui_enabled = boolean(&values, "REMINDI_WEBUI_ENABLE", true)?;
        let webui_auth_enabled = boolean(&values, "REMINDI_WEBUI_AUTH", true)?;
        let credentials_required = webui_enabled && webui_auth_enabled;
        let credential_condition = " when WebUI authentication is enabled";
        let webui_username = optional_secret(
            &values,
            "REMINDI_WEBUI_USERNAME",
            credentials_required,
            credential_condition,
        )?;
        let webui_password = optional_secret(
            &values,
            "REMINDI_WEBUI_PASSWORD",
            credentials_required,
            credential_condition,
        )?;

        Ok(Self {
            database_path,
            owner_id,
            mcp_token: SecretString::from(mcp_token),
            backup_directory,
            allowed_hosts: comma_separated(&values, "REMINDI_HTTP_ALLOWED_HOSTS"),
            allowed_origins: comma_separated(&values, "REMINDI_HTTP_ALLOWED_ORIGINS"),
            log_level: value_or_default(&values, "REMINDI_LOG_LEVEL", DEFAULT_LOG_LEVEL),
            log_content,
            webui_enabled,
            webui_auth_enabled,
            webui_username,
            webui_password,
            webui_session_ttl_seconds: positive_integer(
                &values,
                "REMINDI_WEBUI_SESSION_TTL_SECONDS",
                DEFAULT_SESSION_TTL_SECONDS,
            )?,
            webui_cookie_secure: boolean(&values, "REMINDI_WEBUI_COOKIE_SECURE", false)?,
            webui_title: value_or_default(&values, "REMINDI_WEBUI_TITLE", DEFAULT_WEBUI_TITLE),
            webui_custom_css_file: optional_absolute_path(
                &values,
                "REMINDI_WEBUI_CUSTOM_CSS_FILE",
            )?,
            webui_logo_file: optional_absolute_path(&values, "REMINDI_WEBUI_LOGO_FILE")?,
            webui_favicon_file: optional_absolute_path(&values, "REMINDI_WEBUI_FAVICON_FILE")?,
        })
    }

    /// Returns the absolute SQLite database path.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Returns the fixed owner identity for this process.
    #[must_use]
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// Returns the dedicated MCP bearer secret.
    #[must_use]
    pub fn mcp_token(&self) -> &SecretString {
        &self.mcp_token
    }

    /// Returns the protected backup directory.
    #[must_use]
    pub fn backup_directory(&self) -> &Path {
        &self.backup_directory
    }

    /// Returns the configured Host allowlist.
    #[must_use]
    pub fn allowed_hosts(&self) -> &[String] {
        &self.allowed_hosts
    }

    /// Returns the configured MCP Origin allowlist.
    #[must_use]
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }

    /// Returns the structured logger filter directive.
    #[must_use]
    pub fn log_level(&self) -> &str {
        &self.log_level
    }

    /// Reports whether content logging was explicitly enabled.
    #[must_use]
    pub fn log_content(&self) -> bool {
        self.log_content
    }

    /// Reports whether WebUI routes are enabled.
    #[must_use]
    pub fn webui_enabled(&self) -> bool {
        self.webui_enabled
    }

    /// Reports whether WebUI authentication is enabled.
    #[must_use]
    pub fn webui_auth_enabled(&self) -> bool {
        self.webui_auth_enabled
    }

    /// Returns the WebUI username credential when configured.
    #[must_use]
    pub fn webui_username(&self) -> Option<&SecretString> {
        self.webui_username.as_ref()
    }

    /// Returns the WebUI password credential when configured.
    #[must_use]
    pub fn webui_password(&self) -> Option<&SecretString> {
        self.webui_password.as_ref()
    }

    /// Returns the configured in-memory session lifetime.
    #[must_use]
    pub fn webui_session_ttl_seconds(&self) -> u64 {
        self.webui_session_ttl_seconds
    }

    /// Reports whether WebUI session cookies require HTTPS.
    #[must_use]
    pub fn webui_cookie_secure(&self) -> bool {
        self.webui_cookie_secure
    }

    /// Returns the accessible WebUI brand title.
    #[must_use]
    pub fn webui_title(&self) -> &str {
        &self.webui_title
    }

    /// Returns the optional custom CSS path.
    #[must_use]
    pub fn webui_custom_css_file(&self) -> Option<&Path> {
        self.webui_custom_css_file.as_deref()
    }

    /// Returns the optional custom logo path.
    #[must_use]
    pub fn webui_logo_file(&self) -> Option<&Path> {
        self.webui_logo_file.as_deref()
    }

    /// Returns the optional custom favicon path.
    #[must_use]
    pub fn webui_favicon_file(&self) -> Option<&Path> {
        self.webui_favicon_file.as_deref()
    }

    /// Returns the fixed production listener address.
    #[must_use]
    pub const fn listener_address(&self) -> &'static str {
        LISTEN_ADDRESS
    }
}

fn relevant_environment_pair(
    (key, value): (OsString, OsString),
) -> Option<Result<(String, String), ConfigError>> {
    let key = key.into_string().ok()?;
    if !key.starts_with("REMINDI_") {
        return None;
    }

    Some(
        value
            .into_string()
            .map(|value| (key.clone(), value))
            .map_err(|_| ConfigError::NonUnicode { variable: key }),
    )
}

fn value_or_default(
    values: &HashMap<String, String>,
    variable: &'static str,
    default: &'static str,
) -> String {
    values
        .get(variable)
        .map_or_else(|| default.to_owned(), Clone::clone)
}

fn required(
    values: &HashMap<String, String>,
    variable: &'static str,
    condition: &'static str,
) -> Result<String, ConfigError> {
    values
        .get(variable)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .ok_or(ConfigError::Required {
            variable,
            condition,
        })
}

fn optional_secret(
    values: &HashMap<String, String>,
    variable: &'static str,
    is_required: bool,
    condition: &'static str,
) -> Result<Option<SecretString>, ConfigError> {
    let value = values
        .get(variable)
        .filter(|value| !value.trim().is_empty())
        .cloned();

    if is_required && value.is_none() {
        return Err(ConfigError::Required {
            variable,
            condition,
        });
    }

    Ok(value.map(SecretString::from))
}

fn boolean(
    values: &HashMap<String, String>,
    variable: &'static str,
    default: bool,
) -> Result<bool, ConfigError> {
    match values.get(variable).map(String::as_str) {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(ConfigError::InvalidBoolean { variable }),
    }
}

fn positive_integer(
    values: &HashMap<String, String>,
    variable: &'static str,
    default: u64,
) -> Result<u64, ConfigError> {
    match values.get(variable) {
        None => Ok(default),
        Some(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or(ConfigError::InvalidPositiveInteger { variable }),
    }
}

fn comma_separated(values: &HashMap<String, String>, variable: &'static str) -> Vec<String> {
    values
        .get(variable)
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

fn absolute_path(variable: &'static str, value: String) -> Result<PathBuf, ConfigError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(ConfigError::PathNotAbsolute { variable })
    }
}

fn optional_absolute_path(
    values: &HashMap<String, String>,
    variable: &'static str,
) -> Result<Option<PathBuf>, ConfigError> {
    values
        .get(variable)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .map(|value| absolute_path(variable, value))
        .transpose()
}
