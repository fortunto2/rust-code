//! Universal CLI-to-Chat-Completions proxy.
//!
//! Wraps any coding CLI (claude, gemini, codex) as a localhost
//! OpenAI-compatible Chat Completions endpoint so BAML can use it.
//!
//! Each CLI handles its own auth — no API keys needed.

use serde::Deserialize;
use std::process::Stdio;
use tokio::io::AsyncReadExt;

/// Supported CLI providers.
#[derive(Debug, Clone, Copy)]
pub enum CliProvider {
    Claude,
    Gemini,
    Codex,
}

impl CliProvider {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "claude" | "claude-cli" => Some(Self::Claude),
            "gemini" | "gemini-cli" => Some(Self::Gemini),
            "codex" | "codex-cli" => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn model_name(&self) -> &'static str {
        match self {
            Self::Claude => "claude-cli",
            Self::Gemini => "gemini-cli",
            Self::Codex => "codex-cli",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Claude => "Claude CLI",
            Self::Gemini => "Gemini CLI",
            Self::Codex => "Codex CLI",
        }
    }

    /// Build the CLI command + args for a given prompt.
    fn build_command(&self, prompt: &str) -> (String, Vec<String>) {
        match self {
            Self::Claude => (
                "claude".into(),
                vec![
                    "-p".into(),
                    prompt.into(),
                    "--output-format".into(),
                    "text".into(),
                    "--no-session-persistence".into(),
                    "--max-turns".into(),
                    "1".into(),
                    "--disallowed-tools".into(),
                    "Bash,Edit,Write,Read".into(),
                ],
            ),
            Self::Gemini => (
                "gemini".into(),
                vec![
                    "-p".into(),
                    prompt.into(),
                    "--sandbox".into(),
                    "--output-format".into(),
                    "text".into(),
                ],
            ),
            Self::Codex => ("codex".into(), vec!["exec".into(), prompt.into()]),
        }
    }

    /// Extra env vars to set when spawning the CLI.
    fn extra_env(&self) -> Vec<(&str, &str)> {
        match self {
            // Prevent claude from refusing to run inside another claude session
            Self::Claude => vec![("CLAUDECODE", "")],
            _ => vec![],
        }
    }
}

/// Run CLI with prompt and return output text.
async fn run_cli(provider: CliProvider, prompt: &str) -> Result<String, String> {
    let (cmd, args) = provider.build_command(prompt);

    let mut command = tokio::process::Command::new(&cmd);
    command
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (k, v) in provider.extra_env() {
        command.env(k, v);
    }

    let mut child = command.spawn().map_err(|e| {
        format!(
            "{} not found or failed to start: {}. Is it installed?",
            cmd, e
        )
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut output = String::new();
    if let Some(mut out) = stdout {
        out.read_to_string(&mut output)
            .await
            .map_err(|e| e.to_string())?;
    }

    let mut err_output = String::new();
    if let Some(mut err) = stderr {
        err.read_to_string(&mut err_output)
            .await
            .map_err(|e| e.to_string())?;
    }

    let status = child.wait().await.map_err(|e| e.to_string())?;
    if !status.success() && output.trim().is_empty() {
        return Err(format!(
            "{} exited with {}: {}",
            cmd,
            status,
            err_output.trim()
        ));
    }

    let cleaned = clean_output(provider, &output);
    Ok(cleaned)
}

/// Strip CLI-specific noise (headers, banners) from output.
fn clean_output(provider: CliProvider, raw: &str) -> String {
    match provider {
        CliProvider::Codex => {
            let mut found_separator = false;
            let mut lines = Vec::new();
            for line in raw.lines() {
                if !found_separator {
                    if line.starts_with("--------") || line.starts_with("───") {
                        found_separator = true;
                    }
                    continue;
                }
                if line.starts_with("workdir:")
                    || line.starts_with("model:")
                    || line.starts_with("provider:")
                {
                    continue;
                }
                lines.push(line);
            }
            if lines.is_empty() {
                raw.trim().to_string()
            } else {
                lines.join("\n").trim().to_string()
            }
        }
        CliProvider::Gemini => {
            let lines: Vec<&str> = raw
                .lines()
                .filter(|l| {
                    !l.contains("GOOGLE_API_KEY and GEMINI_API_KEY are set")
                        && !l.starts_with("Loading extension:")
                })
                .collect();
            lines.join("\n").trim().to_string()
        }
        CliProvider::Claude => raw.trim().to_string(),
    }
}

// ============================================================================
// HTTP types (Chat Completions compatible)
// ============================================================================

#[derive(Deserialize)]
struct ChatCompletionRequest {
    #[allow(dead_code)]
    model: Option<String>,
    messages: Vec<ChatMessage>,
}

/// Content can be a string or array of parts (OpenAI multi-part format).
#[derive(Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Deserialize)]
struct ContentPart {
    #[serde(default)]
    text: Option<String>,
}

impl MessageContent {
    fn to_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Deserialize)]
struct ChatMessage {
    role: String,
    content: MessageContent,
}

/// Merge all messages into a single prompt for CLI.
fn messages_to_prompt(messages: &[ChatMessage]) -> String {
    let mut parts = Vec::new();
    for msg in messages {
        let text = msg.content.to_text();
        match msg.role.as_str() {
            "system" => parts.push(format!("[System Instructions]\n{}", text)),
            "user" => parts.push(format!("[User]\n{}", text)),
            "assistant" => parts.push(format!("[Assistant]\n{}", text)),
            other => parts.push(format!("[{}]\n{}", other, text)),
        }
    }
    parts.join("\n\n")
}

// ============================================================================
// Proxy Server
// ============================================================================

/// Start a CLI provider proxy on a random localhost port.
/// Returns (port, join_handle).
pub async fn start_cli_proxy(
    provider: CliProvider,
) -> Result<(u16, tokio::task::JoinHandle<()>), String> {
    // Verify CLI exists
    let (cmd, _) = provider.build_command("test");
    let check = tokio::process::Command::new("which")
        .arg(&cmd)
        .output()
        .await;
    if check.is_err() || !check.unwrap().status.success() {
        return Err(format!(
            "{} CLI not found. Install it first.",
            provider.display_name()
        ));
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let prov = provider;
            tokio::spawn(handle_connection(stream, prov));
        }
    });

    Ok((port, handle))
}

async fn handle_connection(mut stream: tokio::net::TcpStream, provider: CliProvider) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt};

    let mut buf = Vec::with_capacity(131072);
    let mut tmp = vec![0u8; 65536];
    loop {
        let n = match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return,
        };
        buf.extend_from_slice(&tmp[..n]);
        let data = String::from_utf8_lossy(&buf);
        if let Some(header_end) = data.find("\r\n\r\n") {
            let headers = &data[..header_end];
            let body_received = buf.len() - header_end - 4;
            let content_length = headers
                .lines()
                .find_map(|l| {
                    let lower = l.to_lowercase();
                    if lower.starts_with("content-length:") {
                        l.split(':').nth(1)?.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            if body_received >= content_length {
                break;
            }
        }
        if buf.len() > 4 * 1024 * 1024 {
            break;
        }
    }

    let request = String::from_utf8_lossy(&buf);

    // Models endpoint
    if request.starts_with("GET") && request.contains("/v1/models") {
        let models_json = serde_json::json!({
            "object": "list",
            "data": [
                {"id": provider.model_name(), "object": "model", "owned_by": "cli"},
            ]
        });
        let resp_body = serde_json::to_string(&models_json).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    // Find body
    let body_start = match request.find("\r\n\r\n") {
        Some(pos) => pos + 4,
        None => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            return;
        }
    };
    let body = &request[body_start..];

    // Parse Chat Completions request
    let chat_req: ChatCompletionRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            let err = format!("{{\"error\":{{\"message\":\"Invalid request: {}\"}}}}", e);
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                err.len(), err
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    };

    let prompt = messages_to_prompt(&chat_req.messages);

    // Run CLI
    let text = match run_cli(provider, &prompt).await {
        Ok(t) => t,
        Err(e) => {
            let err = format!(
                "{{\"error\":{{\"message\":\"CLI error: {}\"}}}}",
                e.replace('"', "'")
            );
            let response = format!(
                "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                err.len(), err
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let in_tokens = (prompt.len() / 4) as u64;
    let out_tokens = (text.len() / 4) as u64;
    let model = provider.model_name();

    // Check if client requested streaming
    let wants_stream = request.contains("\"stream\"") && request.contains("true");

    if wants_stream {
        // SSE streaming response (BAML expects this)
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")
            .await;

        let chunk = serde_json::json!({
            "id": format!("chatcmpl-cli-{}", now),
            "object": "chat.completion.chunk",
            "created": now,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": text},
                "finish_reason": null
            }]
        });
        let _ = stream
            .write_all(format!("data: {}\n\n", chunk).as_bytes())
            .await;

        let finish = serde_json::json!({
            "id": format!("chatcmpl-cli-{}", now),
            "object": "chat.completion.chunk",
            "created": now,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": in_tokens,
                "completion_tokens": out_tokens,
                "total_tokens": in_tokens + out_tokens
            }
        });
        let _ = stream
            .write_all(format!("data: {}\n\n", finish).as_bytes())
            .await;
        let _ = stream.write_all(b"data: [DONE]\n\n").await;
    } else {
        // Standard JSON response (SGR flexible parser expects this)
        let resp_body = serde_json::json!({
            "id": format!("chatcmpl-cli-{}", now),
            "object": "chat.completion",
            "created": now,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": in_tokens,
                "completion_tokens": out_tokens,
                "total_tokens": in_tokens + out_tokens
            }
        });
        let body_str = serde_json::to_string(&resp_body).unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body_str.len(),
            body_str
        );
        let _ = stream.write_all(response.as_bytes()).await;
    }
}
