//! Connect to configured stdio MCP servers and expose their tools to the model.
// STUB: replaced by the connectors port.

use std::collections::BTreeMap;

use crate::config::McpServerCfg;
use crate::tools::ToolRegistry;

pub struct McpManager;

impl McpManager {
    pub fn new(_servers: &BTreeMap<String, McpServerCfg>) -> Self {
        McpManager
    }

    /// Start every server (30 s timeout each, in parallel) and add their tools to `registry`
    /// as "<server>__<tool>" (non-alphanumerics -> '_'), status "Asking <server>".
    pub async fn start(&self, _registry: &mut ToolRegistry) {}

    /// Names of servers that failed, with the error.
    pub fn errors(&self) -> Vec<(String, String)> {
        vec![]
    }

    pub async fn stop(&self) {}
}
