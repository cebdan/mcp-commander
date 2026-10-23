#[path = "../src/attachments.rs"]
mod attachments;
#[path = "../src/provider.rs"]
mod provider;
#[path = "../src/mcp.rs"]
mod mcp;

use mcp::{ServerConfig, Transport};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let command = std::env::args().nth(1).ok_or_else(|| anyhow::anyhow!("Pass demo_mcp executable path"))?;
    let server = ServerConfig { name: "calculator".into(), transport: Transport::Stdio,
        command, args: String::new(), url: String::new(), enabled: true };
    let (tools, errors) = mcp::discover(&[server]).await;
    anyhow::ensure!(errors.is_empty(), "MCP discovery errors: {errors:?}");
    anyhow::ensure!(tools.len() == 1, "Expected one tool, got {}", tools.len());
    let output = mcp::execute(&tools[0], serde_json::json!({"a": 2, "b": 3})).await?;
    anyhow::ensure!(output.contains('5'), "Wrong MCP result: {output}");
    println!("MCP tool {} returned {output}", tools[0].spec.name);
    Ok(())
}
