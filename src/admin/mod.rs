//! Safe, versioned administration of runtime configuration.

pub mod adapters;
pub mod audit;
pub mod settings;
pub mod workloads;

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    clock::{Clock, IdGenerator},
    config::BootstrapConfig,
    db::DatabaseManager,
    remindi::canonical_timestamp,
};

use self::{
    adapters::{AdapterConfigView, AdapterConfiguration, PublishedAdapters},
    audit::AdminEvent,
    settings::RuntimeSetting,
};

/// Stable admin-core failures safe for an authenticated API to classify.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AdminError {
    #[error("administrative input failed validation")]
    Validation,
    #[error("the expected administrative version is stale")]
    VersionConflict,
    #[error("administrative persistence failed")]
    Database,
}

/// Authenticated actor and request attribution for one attempted mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdminActor {
    actor_id: String,
    request_id: Option<String>,
}

impl AdminActor {
    /// Creates bounded audit attribution that cannot contain control characters.
    pub fn new(
        actor_id: impl Into<String>,
        request_id: Option<String>,
    ) -> Result<Self, AdminError> {
        let actor_id = actor_id.into();
        if actor_id.trim().is_empty()
            || actor_id.len() > 256
            || actor_id.chars().any(char::is_control)
            || request_id.as_ref().is_some_and(|request_id| {
                request_id.is_empty()
                    || request_id.len() > 128
                    || request_id.chars().any(char::is_control)
            })
        {
            return Err(AdminError::Validation);
        }
        Ok(Self {
            actor_id,
            request_id,
        })
    }

    #[must_use]
    pub fn actor_id(&self) -> &str {
        &self.actor_id
    }

    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }
}

/// One environment-owned bootstrap setting exposed without private values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootstrapSettingView {
    pub name: &'static str,
    pub effective_value: Option<Value>,
    pub configured: bool,
    pub redacted: bool,
    pub mutable: bool,
}

/// Redacted, read-only process bootstrap view.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BootstrapView {
    pub settings: Vec<BootstrapSettingView>,
}

/// Database-backed administration core and active adapter publication seam.
pub struct AdminService {
    database: Arc<DatabaseManager>,
    bootstrap: Arc<BootstrapConfig>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    adapters: PublishedAdapters,
    setting_mutation: Mutex<()>,
    adapter_mutation: Mutex<()>,
}

impl AdminService {
    /// Loads and validates persisted administration state before it becomes active.
    pub async fn load(
        database: Arc<DatabaseManager>,
        bootstrap: Arc<BootstrapConfig>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Result<Self, AdminError> {
        let runtime_settings = settings::list(&database).await?;
        if runtime_settings.len() != 11 {
            return Err(AdminError::Validation);
        }
        for setting in &runtime_settings {
            settings::validate_candidate(&runtime_settings, &setting.key, setting.value)?;
        }
        let (_, adapters) = adapters::load(&database, Arc::clone(&clock)).await?;
        Ok(Self {
            database,
            bootstrap,
            clock,
            ids,
            adapters,
            setting_mutation: Mutex::new(()),
            adapter_mutation: Mutex::new(()),
        })
    }

    /// Returns the environment-only bootstrap configuration without private values.
    #[must_use]
    pub fn bootstrap_view(&self) -> BootstrapView {
        BootstrapView {
            settings: vec![
                redacted("REMINDI_DB_PATH", true),
                redacted("REMINDI_OWNER_ID", true),
                secret("REMINDI_MCP_TOKEN", true),
                redacted("REMINDI_BACKUP_DIR", true),
                redacted(
                    "REMINDI_HTTP_ALLOWED_HOSTS",
                    !self.bootstrap.allowed_hosts().is_empty(),
                ),
                redacted(
                    "REMINDI_HTTP_ALLOWED_ORIGINS",
                    !self.bootstrap.allowed_origins().is_empty(),
                ),
                visible(
                    "REMINDI_LOG_LEVEL",
                    Value::String(self.bootstrap.log_level().to_owned()),
                ),
                visible(
                    "REMINDI_LOG_CONTENT",
                    Value::Bool(self.bootstrap.log_content()),
                ),
                visible(
                    "REMINDI_WEBUI_ENABLE",
                    Value::Bool(self.bootstrap.webui_enabled()),
                ),
                visible(
                    "REMINDI_WEBUI_AUTH",
                    Value::Bool(self.bootstrap.webui_auth_enabled()),
                ),
                secret(
                    "REMINDI_WEBUI_USERNAME",
                    self.bootstrap.webui_username().is_some(),
                ),
                secret(
                    "REMINDI_WEBUI_PASSWORD",
                    self.bootstrap.webui_password().is_some(),
                ),
                visible(
                    "REMINDI_WEBUI_SESSION_TTL_SECONDS",
                    Value::from(self.bootstrap.webui_session_ttl_seconds()),
                ),
                visible(
                    "REMINDI_WEBUI_COOKIE_SECURE",
                    Value::Bool(self.bootstrap.webui_cookie_secure()),
                ),
                visible(
                    "REMINDI_WEBUI_TITLE",
                    Value::String(self.bootstrap.webui_title().to_owned()),
                ),
                redacted(
                    "REMINDI_WEBUI_CUSTOM_CSS_FILE",
                    self.bootstrap.webui_custom_css_file().is_some(),
                ),
                redacted(
                    "REMINDI_WEBUI_LOGO_FILE",
                    self.bootstrap.webui_logo_file().is_some(),
                ),
                redacted(
                    "REMINDI_WEBUI_FAVICON_FILE",
                    self.bootstrap.webui_favicon_file().is_some(),
                ),
                visible(
                    "REMINDI_LISTENER_ADDRESS",
                    Value::String(self.bootstrap.listener_address().to_owned()),
                ),
            ],
        }
    }

    /// Lists the exact safe mutable setting inventory.
    pub async fn runtime_settings(&self) -> Result<Vec<RuntimeSetting>, AdminError> {
        settings::list(&self.database).await
    }

    /// Applies one allowlisted runtime setting with optimistic versioning.
    pub async fn update_runtime_setting(
        &self,
        key: &str,
        value: i64,
        expected_version: i64,
        actor: &AdminActor,
    ) -> Result<RuntimeSetting, AdminError> {
        let _mutation = self.setting_mutation.lock().await;
        let known = settings::is_known(key);
        if !known || expected_version < 1 {
            self.rejected(
                "runtime_setting_updated",
                actor,
                setting_details(key, known, Some("VALIDATION_ERROR")),
            )
            .await?;
            return Err(AdminError::Validation);
        }

        let current = settings::list(&self.database).await?;
        if let Err(error) = settings::validate_candidate(&current, key, value) {
            self.rejected(
                "runtime_setting_updated",
                actor,
                setting_details(key, true, Some("VALIDATION_ERROR")),
            )
            .await?;
            return Err(error);
        }
        let selected = current
            .iter()
            .find(|setting| setting.key == key)
            .ok_or(AdminError::Validation)?;
        if selected.version != expected_version {
            self.rejected(
                "runtime_setting_updated",
                actor,
                setting_details(key, true, Some("VERSION_CONFLICT")),
            )
            .await?;
            return Err(AdminError::VersionConflict);
        }

        let occurred_at =
            canonical_timestamp(self.clock.now()).map_err(|_| AdminError::Database)?;
        let updated =
            settings::updated(&current, key, value, actor.actor_id(), occurred_at.clone())?;
        let mut transaction = self
            .database
            .begin_immediate()
            .await
            .map_err(|_| AdminError::Database)?;
        let write = sqlx::query(
            "UPDATE runtime_settings \
             SET value_json = ?, version = version + 1, updated_at = ?, updated_by = ? \
             WHERE setting_key = ? AND version = ?",
        )
        .bind(value.to_string())
        .bind(&occurred_at)
        .bind(actor.actor_id())
        .bind(key)
        .bind(expected_version)
        .execute(transaction.as_mut())
        .await;
        let result = match write {
            Ok(result) => result,
            Err(_) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| AdminError::Database)?;
                self.failed(
                    "runtime_setting_updated",
                    actor,
                    setting_details(key, true, Some("INTERNAL_ERROR")),
                )
                .await?;
                return Err(AdminError::Database);
            }
        };
        if result.rows_affected() != 1 {
            transaction
                .rollback()
                .await
                .map_err(|_| AdminError::Database)?;
            self.rejected(
                "runtime_setting_updated",
                actor,
                setting_details(key, true, Some("VERSION_CONFLICT")),
            )
            .await?;
            return Err(AdminError::VersionConflict);
        }
        audit::insert(
            &mut transaction,
            self.clock.as_ref(),
            self.ids.as_ref(),
            "runtime_setting_updated",
            actor,
            "succeeded",
            &setting_details(key, true, None),
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| AdminError::Database)?;
        Ok(updated)
    }

    /// Lists persisted typed configuration for all four adapters.
    pub async fn adapter_configs(&self) -> Result<Vec<AdapterConfigView>, AdminError> {
        adapters::list(&self.database).await
    }

    /// Validates, persists, and publishes one complete adapter candidate.
    pub async fn update_adapter(
        &self,
        adapter_name: &str,
        enabled: bool,
        configuration: AdapterConfiguration,
        expected_version: i64,
        actor: &AdminActor,
    ) -> Result<AdapterConfigView, AdminError> {
        let _mutation = self.adapter_mutation.lock().await;
        let known = adapters::is_known(adapter_name);
        if !known || expected_version < 1 {
            self.rejected(
                "adapter_config_updated",
                actor,
                adapter_details(adapter_name, known, Some("VALIDATION_ERROR")),
            )
            .await?;
            return Err(AdminError::Validation);
        }
        let current = adapters::list(&self.database).await?;
        let occurred_at =
            canonical_timestamp(self.clock.now()).map_err(|_| AdminError::Database)?;
        let (updated, snapshot) = match adapters::candidate(
            &current,
            adapter_name,
            enabled,
            configuration,
            actor.actor_id(),
            occurred_at.clone(),
            Arc::clone(&self.clock),
        ) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.rejected(
                    "adapter_config_updated",
                    actor,
                    adapter_details(adapter_name, true, Some("VALIDATION_ERROR")),
                )
                .await?;
                return Err(error);
            }
        };
        let selected = current
            .iter()
            .find(|config| config.adapter_name == adapter_name)
            .ok_or(AdminError::Validation)?;
        if selected.version != expected_version {
            self.rejected(
                "adapter_config_updated",
                actor,
                adapter_details(adapter_name, true, Some("VERSION_CONFLICT")),
            )
            .await?;
            return Err(AdminError::VersionConflict);
        }
        let config_json = adapters::encoded(&updated.configuration)?;

        let mut transaction = self
            .database
            .begin_immediate()
            .await
            .map_err(|_| AdminError::Database)?;
        let write = sqlx::query(
            "UPDATE adapter_configs \
             SET enabled = ?, config_json = ?, version = version + 1, \
                 updated_at = ?, updated_by = ? \
             WHERE adapter_name = ? AND version = ?",
        )
        .bind(i64::from(enabled))
        .bind(config_json)
        .bind(&occurred_at)
        .bind(actor.actor_id())
        .bind(adapter_name)
        .bind(expected_version)
        .execute(transaction.as_mut())
        .await;
        let result = match write {
            Ok(result) => result,
            Err(_) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| AdminError::Database)?;
                self.failed(
                    "adapter_config_updated",
                    actor,
                    adapter_details(adapter_name, true, Some("INTERNAL_ERROR")),
                )
                .await?;
                return Err(AdminError::Database);
            }
        };
        if result.rows_affected() != 1 {
            transaction
                .rollback()
                .await
                .map_err(|_| AdminError::Database)?;
            self.rejected(
                "adapter_config_updated",
                actor,
                adapter_details(adapter_name, true, Some("VERSION_CONFLICT")),
            )
            .await?;
            return Err(AdminError::VersionConflict);
        }
        audit::insert(
            &mut transaction,
            self.clock.as_ref(),
            self.ids.as_ref(),
            "adapter_config_updated",
            actor,
            "succeeded",
            &adapter_details(adapter_name, true, None),
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| AdminError::Database)?;
        self.adapters.publish(snapshot)?;
        Ok(updated)
    }

    /// Returns the process-shared adapter snapshot provider.
    #[must_use]
    pub fn adapters(&self) -> PublishedAdapters {
        self.adapters.clone()
    }

    /// Reads immutable administrative events in sequence order.
    pub async fn admin_events(
        &self,
        after_sequence: Option<i64>,
        limit: u16,
    ) -> Result<Vec<AdminEvent>, AdminError> {
        audit::list(&self.database, after_sequence, limit).await
    }

    async fn rejected(
        &self,
        event_type: &'static str,
        actor: &AdminActor,
        details: Value,
    ) -> Result<(), AdminError> {
        audit::append(
            &self.database,
            self.clock.as_ref(),
            self.ids.as_ref(),
            event_type,
            actor,
            "rejected",
            &details,
        )
        .await
    }

    async fn failed(
        &self,
        event_type: &'static str,
        actor: &AdminActor,
        details: Value,
    ) -> Result<(), AdminError> {
        audit::append(
            &self.database,
            self.clock.as_ref(),
            self.ids.as_ref(),
            event_type,
            actor,
            "failed",
            &details,
        )
        .await
    }
}

fn visible(name: &'static str, value: Value) -> BootstrapSettingView {
    BootstrapSettingView {
        name,
        effective_value: Some(value),
        configured: true,
        redacted: false,
        mutable: false,
    }
}

fn redacted(name: &'static str, configured: bool) -> BootstrapSettingView {
    BootstrapSettingView {
        name,
        effective_value: configured.then(|| Value::String("[configured]".to_owned())),
        configured,
        redacted: true,
        mutable: false,
    }
}

fn secret(name: &'static str, configured: bool) -> BootstrapSettingView {
    BootstrapSettingView {
        name,
        effective_value: None,
        configured,
        redacted: true,
        mutable: false,
    }
}

fn setting_details(key: &str, known: bool, failure_code: Option<&str>) -> Value {
    let mut details = serde_json::Map::new();
    details.insert(
        "setting_name".to_owned(),
        Value::String(if known { key } else { "[unknown]" }.to_owned()),
    );
    if let Some(code) = failure_code {
        details.insert("failure_code".to_owned(), Value::String(code.to_owned()));
    }
    Value::Object(details)
}

fn adapter_details(name: &str, known: bool, failure_code: Option<&str>) -> Value {
    let mut details = serde_json::Map::new();
    details.insert(
        "adapter_name".to_owned(),
        Value::String(if known { name } else { "[unknown]" }.to_owned()),
    );
    if let Some(code) = failure_code {
        details.insert("failure_code".to_owned(), Value::String(code.to_owned()));
    }
    Value::Object(details)
}
