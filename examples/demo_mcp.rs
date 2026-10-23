use rmcp::{handler::server::wrapper::Parameters, schemars, tool, tool_router, ServiceExt, transport::stdio};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddParams { a: i32, b: i32 }

#[derive(Clone)]
struct Calculator;

#[tool_router(server_handler)]
impl Calculator {
    #[tool(description = "Add two integers")]
    fn add(&self, Parameters(AddParams { a, b }): Parameters<AddParams>) -> String {
        (a + b).to_string()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    Calculator.serve(stdio()).await?.waiting().await?;
    Ok(())
}
