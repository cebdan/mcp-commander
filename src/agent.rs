use std::sync::mpsc::Sender;
use anyhow::{Result, bail};
use serde_json::json;
use tokio::{sync::oneshot, task::JoinSet};
use crate::{mcp::{self, ServerConfig, ToolBinding}, provider::{self, ChatMessage, Settings, ToolCall, ToolSpec}};

pub const MAX_IMPLEMENTERS: usize = 8;
const MAX_ROUNDS: usize = 8;
const DELEGATE_TOOL: &str = "delegate_to_implementer";

pub enum Event {
    Status(String), Answer(String), Error(String),
    Approval(String, ToolCall, oneshot::Sender<bool>),
    Tools(Vec<String>, Vec<String>), Models(Vec<String>), KeyCheck(u64, bool, String),
    WorkerAssigned(usize, String), WorkerStatus(usize, String), WorkerAnswer(usize, String),
    Done,
}

fn delegate_spec() -> ToolSpec {
    ToolSpec { name: DELEGATE_TOOL.into(),
        description: "Assign a concrete task to an available implementer and wait for its result. Independent calls in one response run in parallel on different implementers.".into(),
        parameters: json!({"type":"object","properties":{
            "implementer":{"type":"integer","description":"One-based implementer number"},
            "task":{"type":"string","description":"Specific work and expected output"}},
            "required":["implementer","task"]}) }
}

fn assignment(call: &ToolCall, available: &[bool]) -> Result<(usize, String)> {
    let id = call.arguments["implementer"].as_u64().unwrap_or(0) as usize;
    let task = call.arguments["task"].as_str().unwrap_or("").trim();
    if id == 0 || id > available.len() { bail!("Implementer number must be between 1 and {}", available.len()); }
    if !available[id - 1] { bail!("Implementer {id} is already assigned in this round"); }
    if task.is_empty() { bail!("Task cannot be empty"); }
    Ok((id - 1, task.to_owned()))
}

fn mcp_instruction(bindings: &[ToolBinding]) -> String {
    if bindings.is_empty() {
        "No MCP tools are connected for this request. If the user asks to use a local app, explain that its MCP server must be connected.".into()
    } else {
        let names = bindings.iter().map(|binding| binding.spec.name.as_str())
            .collect::<Vec<_>>().join(", ");
        format!("These are live tools from connected MCP servers: {names}. MCP includes requests written as 'МСП' in Russian. Do not claim MCP or GraphCAD tools are unavailable when they are listed here. When the user requests an action in a connected app, use the relevant tool instead of merely describing the action. If the user only asks whether MCP is connected, confirm the connection. Never claim an action succeeded until its tool result confirms it.")
    }
}

async fn next_nonempty(conversation: &mut provider::Conversation, specs: &[ToolSpec]) -> Result<provider::Reply> {
    let mut reply = conversation.next(specs).await?;
    if reply.calls.is_empty() && reply.text.trim().is_empty() {
        conversation.request_visible_answer();
        reply = conversation.next(specs).await?;
        if reply.calls.is_empty() && reply.text.trim().is_empty() {
            bail!("The model returned no visible answer or tool call twice. Try another model or start a new chat.");
        }
    }
    Ok(reply)
}

pub async fn run(settings: Settings, history: Vec<ChatMessage>, servers: Vec<ServerConfig>,
    auto_approve_mcp: bool, implementers: Vec<Settings>, tx: Sender<Event>) -> Result<()> {
    let (bindings, errors) = mcp::discover(&servers).await;
    let _ = tx.send(Event::Tools(bindings.iter().map(|b| b.spec.name.clone()).collect(), errors));
    let result = if implementers.is_empty() {
        run_dialog(settings, history, bindings, auto_approve_mcp, None, tx.clone()).await?
    } else {
        run_commander(settings, history, bindings, auto_approve_mcp,
            implementers, tx.clone()).await?
    };
    let _ = tx.send(Event::Answer(result));
    Ok(())
}

async fn run_commander(settings: Settings, history: Vec<ChatMessage>, bindings: Vec<ToolBinding>,
    auto_approve_mcp: bool, implementers: Vec<Settings>, tx: Sender<Event>) -> Result<String> {
    let implementer_count = implementers.len().min(MAX_IMPLEMENTERS);
    let roster = implementers.iter().take(implementer_count).enumerate()
        .map(|(id, settings)| format!("{}: {} / {}", id + 1, settings.provider.label(), settings.model))
        .collect::<Vec<_>>().join("; ");
    let instructions = format!("You coordinate {implementer_count} implementers. Roster: {roster}. Use delegate_to_implementer to assign concrete tasks to available numbered agents. Choose an implementer whose model fits the task. Review returned results and give the user a final answer. Issue multiple delegate_to_implementer calls in one response for independent tasks. Do not claim work is complete until the tool result confirms it.");
    let attachments = history.iter().rev().find(|m| m.role == "user")
        .map(|m| m.attachments.clone()).unwrap_or_default();
    let mut conversation = provider::Conversation::new_with_instruction(settings.clone(),
        &history, Some(format!("You are the commander-agent. {instructions} {}", mcp_instruction(&bindings))))?;
    let mut specs: Vec<_> = bindings.iter().map(|b| b.spec.clone()).collect();
    specs.push(delegate_spec());
    for _ in 0..MAX_ROUNDS {
        let _ = tx.send(Event::Status("Commander is planning...".into()));
        let reply = next_nonempty(&mut conversation, &specs).await?;
        if reply.calls.is_empty() {
            return Ok(reply.text);
        }
        let mut available = vec![true; implementer_count];
        let mut results: Vec<Option<String>> = vec![None; reply.calls.len()];
        let mut workers = JoinSet::new();
        for (index, call) in reply.calls.iter().enumerate() {
            if call.name == DELEGATE_TOOL {
                match assignment(call, &available) {
                    Ok((id, task)) => {
                        available[id] = false;
                        let _ = tx.send(Event::WorkerAssigned(id, task.clone()));
                        let settings = implementers[id].clone();
                        let bindings = bindings.clone();
                        let tx = tx.clone();
                        let attachments = attachments.clone();
                        workers.spawn(async move {
                            let history = vec![ChatMessage { role: "user".into(), content: task, attachments }];
                            let result = run_dialog(settings, history, bindings, auto_approve_mcp,
                                Some(id), tx.clone()).await;
                            (index, id, result)
                        });
                    }
                    Err(error) => results[index] = Some(format!("Delegation rejected: {error}")),
                }
            } else {
                results[index] = Some(execute_tool(call, &bindings, auto_approve_mcp,
                    "Commander", &tx).await);
            }
        }
        while let Some(joined) = workers.join_next().await {
            let (index, id, result) = joined?;
            let answer = match result { Ok(answer) => answer,
                Err(error) => format!("Implementer failed: {error:#}") };
            let _ = tx.send(Event::WorkerAnswer(id, answer.clone()));
            results[index] = Some(answer);
        }
        let completed: Vec<_> = reply.calls.into_iter().zip(results).map(|(call, output)|
            (call, output.unwrap_or_else(|| "No result".into()))).collect();
        conversation.add_results(&completed);
    }
    Ok("Stopped after eight tool rounds. Ask me to continue if needed.".into())
}

async fn run_dialog(settings: Settings, history: Vec<ChatMessage>, bindings: Vec<ToolBinding>,
    auto_approve_mcp: bool, worker: Option<usize>, tx: Sender<Event>) -> Result<String> {
    let role_instruction = if worker.is_some() {
        "You are an implementer. Complete the assigned task using available MCP tools where needed. Report exactly what you did and any errors. You cannot delegate to other agents."
    } else {
        "You are an assistant that can use the connected MCP tools."
    };
    let mut conversation = provider::Conversation::new_with_instruction(settings, &history,
        Some(format!("{role_instruction} {}", mcp_instruction(&bindings))))?;
    let specs: Vec<_> = bindings.iter().map(|b| b.spec.clone()).collect();
    for _ in 0..MAX_ROUNDS {
        match worker { Some(id) => { let _ = tx.send(Event::WorkerStatus(id, "Waiting for model...".into())); },
            None => { let _ = tx.send(Event::Status("Waiting for model...".into())); } }
        let reply = next_nonempty(&mut conversation, &specs).await?;
        if reply.calls.is_empty() {
            return Ok(reply.text);
        }
        let mut results = Vec::new();
        for call in reply.calls {
            let source = worker.map(|id| format!("Implementer {}", id + 1))
                .unwrap_or_else(|| "Agent".into());
            let output = execute_tool(&call, &bindings, auto_approve_mcp, &source, &tx).await;
            if let Some(id) = worker { let _ = tx.send(Event::WorkerStatus(id, format!("Used {}", call.name))); }
            results.push((call, output));
        }
        conversation.add_results(&results);
    }
    Ok("Stopped after eight tool rounds. Ask me to continue if needed.".into())
}

async fn execute_tool(call: &ToolCall, bindings: &[ToolBinding], auto_approve_mcp: bool,
    source: &str, tx: &Sender<Event>) -> String {
    let allowed = if auto_approve_mcp { true } else {
        let (decision_tx, decision_rx) = oneshot::channel();
        if tx.send(Event::Approval(source.into(), call.clone(), decision_tx)).is_err() {
            return "Approval unavailable".into();
        }
        decision_rx.await.unwrap_or(false)
    };
    if !allowed { return "User denied this tool call".into(); }
    let _ = tx.send(Event::Status(format!("{source}: running {}...", call.name)));
    match bindings.iter().find(|b| b.spec.name == call.name) {
        Some(binding) => match mcp::execute(binding, call.arguments.clone()).await {
            Ok(output) => output, Err(error) => format!("Tool error: {error:#}") },
        None => "Tool unavailable".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::{Read, Write}, net::TcpListener};
    use crate::provider::Provider;
    #[test]
    fn assignment_rejects_busy_and_invalid_workers() {
        let call = ToolCall { id: "1".into(), name: DELEGATE_TOOL.into(),
            arguments: json!({"implementer": 2, "task": " build the model "}) };
        assert_eq!(assignment(&call, &[true, true]).unwrap(), (1, "build the model".into()));
        assert!(assignment(&call, &[true, false]).is_err());
        assert!(assignment(&call, &[true]).is_err());
    }

    #[tokio::test]
    async fn empty_ollama_reply_is_retried_with_mcp_context() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut chunk = [0u8; 4096];
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break end + 4; }
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let length: usize = headers.lines().find_map(|line| line.to_ascii_lowercase()
                    .strip_prefix("content-length: ").and_then(|s| s.trim().parse().ok())).unwrap();
                while bytes.len() - header_end < length {
                    let mut chunk = [0u8; 4096];
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let request: serde_json::Value = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                assert_eq!(request["messages"][0]["role"], "system");
                assert!(request["messages"][0]["content"].as_str().unwrap().contains("graph_cad_get_graph"));
                assert_eq!(request["tools"][0]["function"]["name"], "graph_cad_get_graph");
                if attempt == 1 {
                    assert!(request["messages"].to_string().contains("no visible answer"));
                }
                let reply = if attempt == 0 {
                    json!({"message":{"role":"assistant","content":"","thinking":"Reasoning only"}})
                } else {
                    json!({"message":{"role":"assistant","content":"MCP is connected."}})
                };
                let body = reply.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        let binding = ToolBinding {
            spec: ToolSpec { name: "graph_cad_get_graph".into(), description: "Read the graph".into(),
                parameters: json!({"type":"object"}) },
            server: ServerConfig::default(), original_name: "get_graph".into(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let result = run_dialog(Settings { provider: Provider::Ollama, model: "test".into(),
            base_url: format!("http://{address}"), api_key: String::new() },
            vec![ChatMessage { role: "user".into(), content: "Is MCP connected?".into(), attachments: vec![] }],
            vec![binding], false, None, tx).await.unwrap();
        server.join().unwrap();
        assert_eq!(result, "MCP is connected.");
        assert!(!rx.try_iter().any(|event| matches!(event, Event::Approval(..))));
    }

    #[tokio::test]
    async fn commander_dispatches_two_implementers_and_collects_results() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let mut stream = stream.unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut chunk = [0u8; 4096];
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break end + 4; }
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let length: usize = headers.lines().find_map(|line| line.to_ascii_lowercase()
                    .strip_prefix("content-length: ").and_then(|s| s.trim().parse().ok())).unwrap();
                while bytes.len() - header_end < length {
                    let mut chunk = [0u8; 4096];
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let request: serde_json::Value = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                let messages = request["messages"].to_string();
                let is_commander = request["tools"].to_string().contains(DELEGATE_TOOL);
                let reply = if is_commander && !messages.contains("tool_call_id") {
                    json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
                        {"id":"a","type":"function","function":{"name":DELEGATE_TOOL,"arguments":"{\"implementer\":1,\"task\":\"Do A\"}"}},
                        {"id":"b","type":"function","function":{"name":DELEGATE_TOOL,"arguments":"{\"implementer\":2,\"task\":\"Do B\"}"}}
                    ]}}]})
                } else if is_commander {
                    assert!(messages.contains("Finished A"));
                    assert!(messages.contains("Finished B"));
                    json!({"choices":[{"message":{"role":"assistant","content":"Both tasks completed"}}]})
                } else {
                    let answer = match request["model"].as_str().unwrap() {
                        "worker-a" => "Finished A",
                        "worker-b" => "Finished B",
                        model => panic!("Unexpected implementer model: {model}"),
                    };
                    json!({"choices":[{"message":{"role":"assistant","content":answer}}]})
                };
                let body = reply.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        let (tx, rx) = std::sync::mpsc::channel();
        run(Settings { provider: Provider::OpenAI, model: "commander".into(),
            base_url: format!("http://{address}"), api_key: "test".into() },
            vec![ChatMessage { role: "user".into(), content: "Do A and B".into(), attachments: vec![] }],
            vec![], false, ["worker-a", "worker-b"].into_iter().map(|model| Settings {
                provider: Provider::OpenAI, model: model.into(),
                base_url: format!("http://{address}"), api_key: "test".into(),
            }).collect(), tx).await.unwrap();
        server.join().unwrap();
        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!(events.iter().filter(|e| matches!(e, Event::WorkerAssigned(..))).count(), 2);
        assert_eq!(events.iter().filter(|e| matches!(e, Event::WorkerAnswer(..))).count(), 2);
        assert!(events.iter().any(|e| matches!(e, Event::Answer(s) if s == "Both tasks completed")));
    }
}
