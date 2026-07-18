//! MCP transport data contracts shared by the eight public tools.

pub mod responses;
pub mod schemas;
pub mod server;
pub mod tools;
mod views;

use std::sync::Arc;

use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ErrorCode as McpErrorCode, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
};
use serde_json::Value;

use crate::remindi::{Actor, RemindiService};

/// Factory for per-call authenticated actor and request context.
pub type ActorFactory = Arc<dyn Fn() -> Actor + Send + Sync>;

/// Remindi's MCP server surface backed by the shared application service.
pub struct McpServer {
    service: Arc<RemindiService>,
    actor_factory: ActorFactory,
}

impl McpServer {
    /// Creates a server whose caller identity is supplied by the authenticated transport.
    #[must_use]
    pub fn new<F>(service: Arc<RemindiService>, actor_factory: F) -> Self
    where
        F: Fn() -> Actor + Send + Sync + 'static,
    {
        Self {
            service,
            actor_factory: Arc::new(actor_factory),
        }
    }

    /// Returns the exact ordered public tool catalog.
    #[must_use]
    pub fn tool_definitions() -> Vec<Tool> {
        tools::definitions()
    }

    /// Executes one known tool using already-authenticated caller context.
    pub async fn execute(&self, name: &str, arguments: Value) -> CallToolResult {
        tools::execute(self, name, arguments).await
    }

    pub(crate) fn service(&self) -> &RemindiService {
        &self.service
    }

    pub(crate) fn actor(&self) -> Actor {
        (self.actor_factory)()
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: Self::tool_definitions(),
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        Self::tool_definitions()
            .into_iter()
            .find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if self.get_tool(&request.name).is_none() {
            return Err(ErrorData::new(
                McpErrorCode::METHOD_NOT_FOUND,
                "unknown Remindi tool",
                None,
            ));
        }
        Ok(self
            .execute(
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
            )
            .await)
    }
}
