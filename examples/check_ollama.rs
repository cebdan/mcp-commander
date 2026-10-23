#[path = "../src/attachments.rs"]
mod attachments;
#[path = "../src/provider.rs"]
mod provider;

use provider::{ChatMessage, Conversation, Provider, Settings};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).unwrap_or_else(|| Provider::Ollama.default_url().to_owned());
    let models = provider::ollama_models(&url).await?;
    println!("Found {} Ollama models", models.len());
    let model = models.iter().find(|m| m.as_str() == "qwen3:4b")
        .or_else(|| models.first()).ok_or_else(|| anyhow::anyhow!("No Ollama models installed"))?;
    let mut conversation = Conversation::new_with_instruction(Settings {
        provider: Provider::Ollama, model: model.clone(), base_url: url, api_key: String::new(),
    }, &[ChatMessage { role: "user".into(), content: "Reply with OK only.".into(), attachments: vec![] }], None)?;
    let reply = conversation.next(&[]).await?;
    println!("Model {} replied: {}", model, reply.text);
    Ok(())
}
