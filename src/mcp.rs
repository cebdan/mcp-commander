use anyhow::{Context, Result, bail};
use rmcp::{ServiceExt, model::CallToolRequestParams, transport::{StreamableHttpClientTransport, TokioChildProcess}};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::process::Command;

use crate::provider::ToolSpec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    #[default]
    Stdio,
    Http,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ServerConfig {
    pub name: String,
    pub transport: Transport,
    pub command: String,
    pub args: String,
    pub url: String,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct ToolBinding {
    pub spec: ToolSpec,
    pub server: ServerConfig,
    pub original_name: String,
}

pub async fn discover(servers: &[ServerConfig]) -> (Vec<ToolBinding>, Vec<String>) {
    let mut tools = Vec::new();
    let mut errors = Vec::new();
    for server in servers.iter().filter(|s| s.enabled) {
        match list_server_tools(server).await {
            Ok(found) => {
                for tool in found {
                    let original_name = tool["name"].as_str().unwrap_or("").to_owned();
                    if original_name.is_empty() { continue; }
                    let prefix = safe_name(&server.name);
                    let function_name = format!("{}_{}", prefix, safe_name(&original_name));
                    if tools.iter().any(|t: &ToolBinding| t.spec.name == function_name) {
                        errors.push(format!("Duplicate MCP tool: {function_name}"));
                        continue;
                    }
                    tools.push(ToolBinding {
                        spec: ToolSpec {
                            name: function_name,
                            description: tool["description"].as_str().unwrap_or("").to_owned(),
                            parameters: tool.get("inputSchema").cloned().unwrap_or_else(|| serde_json::json!({"type":"object"})),
                        },
                        server: server.clone(),
                        original_name,
                    });
                }
            }
            Err(e) => errors.push(format!("{}: {e:#}", server.name)),
        }
    }
    (tools, errors)
}

async fn list_server_tools(server: &ServerConfig) -> Result<Vec<Value>> {
    match server.transport {
        Transport::Stdio => {
            let transport = child_transport(server)?;
            let client = ().serve(transport).await.context("MCP connection failed")?;
            let result = client.list_all_tools().await.context("Cannot list MCP tools")?;
            client.cancel().await?;
            result.into_iter().map(|t| serde_json::to_value(t).map_err(Into::into)).collect()
        }
        Transport::Http => {
            if server.url.trim().is_empty() { bail!("MCP URL is empty"); }
            let transport = StreamableHttpClientTransport::from_uri(server.url.trim());
            let client = ().serve(transport).await.context("MCP HTTP connection failed")?;
            let result = client.list_all_tools().await.context("Cannot list MCP tools")?;
            client.cancel().await?;
            result.into_iter().map(|t| serde_json::to_value(t).map_err(Into::into)).collect()
        }
    }
}

pub async fn execute(binding: &ToolBinding, arguments: Value) -> Result<String> {
    let args: Map<String, Value> = arguments.as_object().cloned().context("Tool arguments must be an object")?;
    let request = CallToolRequestParams::new(binding.original_name.clone()).with_arguments(args);
    let result = match binding.server.transport {
        Transport::Stdio => {
            let transport = child_transport(&binding.server)?;
            let client = ().serve(transport).await.context("MCP connection failed")?;
            let result = client.call_tool(request).await.context("MCP tool call failed")?;
            client.cancel().await?;
            result
        }
        Transport::Http => {
            let transport = StreamableHttpClientTransport::from_uri(binding.server.url.trim());
            let client = ().serve(transport).await.context("MCP HTTP connection failed")?;
            let result = client.call_tool(request).await.context("MCP tool call failed")?;
            client.cancel().await?;
            result
        }
    };
    let output = serde_json::to_string(&result)?;
    Ok(output.chars().take(20_000).collect())
}

fn child_transport(server: &ServerConfig) -> Result<TokioChildProcess> {
    if server.command.trim().is_empty() { bail!("MCP command is empty"); }
    let mut command = Command::new(server.command.trim());
    command.args(server.args.lines().map(str::trim).filter(|s| !s.is_empty()));
    TokioChildProcess::new(command).context("Cannot start MCP process")
}

fn safe_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(55).collect();
    if cleaned.is_empty() { "mcp".to_owned() } else { cleaned }
}
