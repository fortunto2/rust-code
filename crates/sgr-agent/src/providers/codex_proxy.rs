//! Lightweight proxy: OpenAI Chat Completions API → ChatGPT Codex Responses API.
//!
//! Accepts standard `/v1/chat/completions` requests on localhost,
//! converts them to Codex Responses format, forwards to chatgpt.com,
//! and returns a Chat Completions-compatible response.
//!
//! This allows BAML (which speaks Chat Completions) to use a ChatGPT Plus/Pro
//! subscription via the Codex Responses endpoint — no API key needed.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

const CODEX_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const JWT_CLAIM: &str = "https://api.openai.com/auth";

// ============================================================================
// Token Management
// ============================================================================

#[derive(Clone)]
pub struct CodexAuth {
    inner: Arc<RwLock<CodexAuthInner>>,
}

struct CodexAuthInner {
    access_token: String,
    refresh_token: String,
    account_id: String,
    expires_at: u64,
}

impl CodexAuth {
    /// Load from ~/.codex/auth.json and refresh token immediately.
    pub async fn from_codex_config() -> Result<Self, String> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let path = std::path::PathBuf::from(&home).join(".codex/auth.json");
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
        let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let refresh = json["tokens"]["refresh_token"]
            .as_str()
            .ok_or_else(|| format!("No refresh_token in {}", path.display()))?
            .to_string();

        let auth = Self::refresh_token(&refresh).await?;
        Ok(Self {
            inner: Arc::new(RwLock::new(auth)),
        })
    }

    async fn refresh_token(refresh: &str) -> Result<CodexAuthInner, String> {
        let client = reqwest::Client::new();
        let resp = client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "grant_type=refresh_token&refresh_token={}&client_id={}",
                refresh, CLIENT_ID
            ))
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("Token refresh failed: {}", text));
        }

        let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        let access = json["access_token"]
            .as_str()
            .ok_or_else(|| "No access_token in refresh response".to_string())?
            .to_string();
        let new_refresh = json["refresh_token"]
            .as_str()
            .ok_or_else(|| "No refresh_token in refresh response".to_string())?
            .to_string();
        let expires_in = json["expires_in"].as_u64().unwrap_or(864000);

        let account_id = extract_account_id(&access)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Ok(CodexAuthInner {
            access_token: access,
            refresh_token: new_refresh,
            account_id,
            expires_at: now + expires_in,
        })
    }

    /// Get valid access token, auto-refreshing if expired.
    async fn get_token(&self) -> Result<(String, String), String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        {
            let inner = self.inner.read().await;
            if now < inner.expires_at.saturating_sub(60) {
                return Ok((inner.access_token.clone(), inner.account_id.clone()));
            }
        }

        // Token expired, refresh
        let refresh;
        {
            let inner = self.inner.read().await;
            refresh = inner.refresh_token.clone();
        }

        let new_inner = Self::refresh_token(&refresh).await?;
        let token = new_inner.access_token.clone();
        let account_id = new_inner.account_id.clone();
        {
            let mut inner = self.inner.write().await;
            *inner = new_inner;
        }
        Ok((token, account_id))
    }
}

fn extract_account_id(token: &str) -> Result<String, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("Invalid JWT".into());
    }
    use base64::Engine;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload_bytes = engine.decode(parts[1]).map_err(|e| e.to_string())?;
    let payload: serde_json::Value =
        serde_json::from_slice(&payload_bytes).map_err(|e| e.to_string())?;
    payload[JWT_CLAIM]["chatgpt_account_id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "No chatgpt_account_id in JWT".into())
}

// ============================================================================
// Request/Response types
// ============================================================================

#[derive(Deserialize)]
struct ChatCompletionRequest {
    model: Option<String>,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    temperature: Option<f64>,
}

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
    fn as_text(&self) -> String {
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

#[derive(Serialize)]
struct CodexRequest {
    model: String,
    store: bool,
    stream: bool,
    instructions: String,
    input: Vec<CodexInput>,
    text: CodexText,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Serialize)]
struct CodexInput {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct CodexText {
    verbosity: String,
}

// ============================================================================
// Conversion
// ============================================================================

fn chat_to_codex(req: &ChatCompletionRequest) -> CodexRequest {
    let mut instructions = String::new();
    let mut input = Vec::new();

    for msg in &req.messages {
        let text = msg.content.as_text();
        if msg.role == "system" {
            if !instructions.is_empty() {
                instructions.push('\n');
            }
            instructions.push_str(&text);
        } else {
            input.push(CodexInput {
                role: msg.role.clone(),
                content: text,
            });
        }
    }

    if instructions.is_empty() {
        instructions = "You are a helpful assistant.".to_string();
    }

    CodexRequest {
        model: req
            .model
            .clone()
            .unwrap_or_else(|| "gpt-5.3-codex".to_string()),
        store: false,
        stream: true,
        instructions,
        input,
        text: CodexText {
            verbosity: "medium".to_string(),
        },
        temperature: req.temperature,
    }
}

/// Parse SSE stream from Codex Responses API and extract final text + usage.
async fn parse_codex_sse(resp: reqwest::Response) -> Result<(String, u64, u64), String> {
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let mut output_text = String::new();
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;

    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                break;
            }
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                let event_type = event["type"].as_str().unwrap_or("");

                if event_type == "response.output_text.done" {
                    if let Some(t) = event["text"].as_str() {
                        output_text = t.to_string();
                    }
                }

                if event_type == "response.completed" {
                    if let Some(usage) = event["response"]["usage"].as_object() {
                        input_tokens = usage
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        output_tokens = usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                    }
                    if output_text.is_empty() {
                        if let Some(outputs) = event["response"]["output"].as_array() {
                            for o in outputs {
                                if let Some(contents) = o["content"].as_array() {
                                    for c in contents {
                                        if let Some(t) = c["text"].as_str() {
                                            output_text.push_str(t);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok((output_text, input_tokens, output_tokens))
}

// ============================================================================
// Proxy Server
// ============================================================================

/// Start the Codex proxy on a random localhost port.
/// Returns (port, join_handle).
pub async fn start_codex_proxy() -> Result<(u16, tokio::task::JoinHandle<()>), String> {
    let auth = CodexAuth::from_codex_config().await?;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    let handle = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let auth = auth.clone();
            tokio::spawn(handle_codex_connection(stream, auth));
        }
    });

    Ok((port, handle))
}

async fn handle_codex_connection(mut stream: tokio::net::TcpStream, auth: CodexAuth) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    let body_start = match request.find("\r\n\r\n") {
        Some(pos) => pos + 4,
        None => {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            return;
        }
    };
    let body = &request[body_start..];

    // Models endpoint
    if request.starts_with("GET") && request.contains("/v1/models") {
        let models_json = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-5.3-codex", "object": "model", "owned_by": "openai"},
                {"id": "gpt-5.1-codex-mini", "object": "model", "owned_by": "openai"},
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

    // Parse Chat Completions request
    let chat_req: ChatCompletionRequest = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            let err = format!("{{\"error\":{{\"message\":\"Invalid request: {}\"}}}}", e);
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                err.len(),
                err
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    };

    let codex_req = chat_to_codex(&chat_req);
    let codex_body = serde_json::to_string(&codex_req).unwrap();

    let (token, account_id) = match auth.get_token().await {
        Ok(t) => t,
        Err(e) => {
            let err = format!(
                "{{\"error\":{{\"message\":\"Auth failed: {}\"}}}}",
                e.replace('"', "'")
            );
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                err.len(),
                err
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    };

    let client = reqwest::Client::new();
    let codex_resp = match client
        .post(CODEX_URL)
        .header("Authorization", format!("Bearer {}", token))
        .header("chatgpt-account-id", &account_id)
        .header("OpenAI-Beta", "responses=experimental")
        .header("Content-Type", "application/json")
        .header("accept", "text/event-stream")
        .body(codex_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let err = format!(
                "{{\"error\":{{\"message\":\"Codex API error: {}\"}}}}",
                e.to_string().replace('"', "'")
            );
            let response = format!(
                "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                err.len(),
                err
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    };

    if !codex_resp.status().is_success() {
        let status = codex_resp.status().as_u16();
        let err_text = codex_resp.text().await.unwrap_or_default();
        let err = format!(
            "{{\"error\":{{\"message\":\"Codex returned {}: {}\"}}}}",
            status,
            err_text
                .replace('"', "'")
                .chars()
                .take(200)
                .collect::<String>()
        );
        let response = format!(
            "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            err.len(),
            err
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    let (text, in_tok, out_tok) = match parse_codex_sse(codex_resp).await {
        Ok(r) => r,
        Err(e) => {
            let err = format!(
                "{{\"error\":{{\"message\":\"SSE parse error: {}\"}}}}",
                e.replace('"', "'")
            );
            let response = format!(
                "HTTP/1.1 500 Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                err.len(),
                err
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let model = codex_req.model;

    let _ = stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")
        .await;

    let chunk = serde_json::json!({
        "id": format!("chatcmpl-codex-{}", now),
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
        "id": format!("chatcmpl-codex-{}", now),
        "object": "chat.completion.chunk",
        "created": now,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": in_tok,
            "completion_tokens": out_tok,
            "total_tokens": in_tok + out_tok
        }
    });
    let _ = stream
        .write_all(format!("data: {}\n\n", finish).as_bytes())
        .await;
    let _ = stream.write_all(b"data: [DONE]\n\n").await;
}
