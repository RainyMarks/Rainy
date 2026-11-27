//! MCP servers from pasted JSON (`src/renderer/settings/mcp-json.ts`): the `mcpServers` files of Claude Desktop, Claude
//! Code, Cursor, Windsurf, Cline, Roo Code, Cherry Studio, Trae, Kiro and Gemini CLI, VS Code's `servers`, Zed's
//! `context_servers`, opencode's `mcp`, Continue's server list and the snippets MCP server READMEs show, read into
//! Rainy's own servers. Text is read as the renderer reads it: `JSON.parse`, JavaScript's whitespace, key order and
//! number text. Messages are the keys of the renderer's settings messages with their values.

mod js_json;
mod mask;
mod normalize;
mod repair;
#[cfg(test)]
mod tests;

use serde::ser::{Serialize, SerializeMap, Serializer};

pub use self::js_json::is_blank;
pub use self::mask::{masked_command, masked_url};
pub use self::normalize::{
    clean_server_name, decode_config_file, import_key, looks_like_mcp_json, parse_mcp_json, read_config_file,
    unnamed_server_hint,
};

/// Longest text read, in UTF-16 units as JavaScript counts them.
pub const MCP_JSON_MAX_LENGTH: usize = 512 * 1024;
/// Most servers one import adds.
pub const MCP_JSON_MAX_SERVERS: usize = 64;
/// Most code blocks, or `{…}` blocks in prose, read from one text.
pub(super) const MAX_BLOCKS: usize = 64;
/// Values nested deeper than this are kept as `null`: no server field lies that deep.
pub(super) const MAX_DEPTH: usize = 128;

/// Every message key the reading gives, and the one for a file that cannot be read.
pub const MESSAGE_KEYS: &[&str] = &[
    "mcpJsonEmpty",
    "mcpJsonTooLarge",
    "mcpJsonInvalid",
    "mcpJsonNoServers",
    "mcpJsonTooMany",
    "mcpJsonToml",
    "mcpJsonYaml",
    "mcpJsonCommandLine",
    "mcpJsonServerInvalid",
    "mcpJsonUnknownType",
    "mcpJsonBadUrl",
    "mcpJsonBadHeader",
    "mcpJsonNoCommand",
    "mcpJsonBadArg",
    "mcpJsonBadEnv",
    "mcpJsonRenamed",
    "mcpJsonPlaceholder",
    "mcpJsonCommandSplit",
    "mcpJsonUnwrapped",
    "mcpJsonCwd",
    "mcpJsonEnvFile",
    "mcpJsonExcludeTools",
    "mcpJsonNoTools",
    "mcpJsonInlineScript",
    "mcpJsonDuplicate",
    "mcpJsonOAuth",
    "mcpJsonForeignPath",
    "mcpJsonBackslashes",
    "mcpImportFileError",
];

/// A message key with its values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpImportMessage {
    pub key: &'static str,
    pub vars: Vec<(&'static str, String)>,
}

impl McpImportMessage {
    pub(super) fn new(key: &'static str) -> Self {
        Self { key, vars: Vec::new() }
    }

    pub(super) fn with(key: &'static str, vars: &[(&'static str, &str)]) -> Self {
        Self { key, vars: vars.iter().map(|(name, value)| (*name, (*value).to_owned())).collect() }
    }

    /// The value of one variable.
    #[cfg(test)]
    pub(super) fn var(&self, name: &str) -> Option<&str> {
        self.vars.iter().find(|(var, _)| *var == name).map(|(_, value)| value.as_str())
    }
}

/// A server as the Host saves it (`McpServerConfig` of `src/shared/rpc.ts`); the environment and headers keep their
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub enabled: bool,
    /// `stdio` or `streamable-http`.
    pub transport: String,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<Vec<(String, String)>>,
    pub url: Option<String>,
    pub headers: Option<Vec<(String, String)>>,
    pub tools: Vec<String>,
}

pub(super) struct Pairs<'a>(pub(super) &'a [(String, String)]);

impl Serialize for Pairs<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Serialize for McpServerConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("name", &self.name)?;
        map.serialize_entry("enabled", &self.enabled)?;
        map.serialize_entry("transport", &self.transport)?;
        if let Some(command) = &self.command {
            map.serialize_entry("command", command)?;
        }
        if let Some(args) = &self.args {
            map.serialize_entry("args", args)?;
        }
        if let Some(env) = &self.env {
            map.serialize_entry("env", &Pairs(env))?;
        }
        if let Some(url) = &self.url {
            map.serialize_entry("url", url)?;
        }
        if let Some(headers) = &self.headers {
            map.serialize_entry("headers", &Pairs(headers))?;
        }
        map.serialize_entry("tools", &self.tools)?;
        map.end()
    }
}

/// A configured server, or just its name.
#[derive(Debug, Clone)]
pub enum ExistingServer {
    Name(String),
    Server(McpServerConfig),
}

impl ExistingServer {
    pub(super) fn name(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Server(server) => &server.name,
        }
    }
}

/// One server read from the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedServer {
    /// The name the text gave it; empty when it gave none.
    pub source: String,
    /// What is saved; its name is cleaned up and kept clear of the configured servers.
    pub server: McpServerConfig,
    /// What the user should know: a new name, an ignored field, a placeholder still to fill in.
    pub notes: Vec<McpImportMessage>,
}

/// The servers read with what was noticed about the whole text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpImport {
    pub servers: Vec<ImportedServer>,
    /// Entries that could not be read, each with its reason.
    pub problems: Vec<McpImportMessage>,
    /// Repairs made to the text itself.
    pub notices: Vec<McpImportMessage>,
}

/// Where the Host runs the servers' commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpImportEnvironment {
    Windows,
    Wsl,
    Ssh,
}
