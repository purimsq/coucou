// Claude API client — the same integration as ClaudeService.swift: multi-turn
// chat with web search, and files sent as document/image/text blocks.
//
// Everything happens here rather than in the island: the API key never leaves
// the Credential Manager, and file bytes never cross the IPC boundary.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::secrets;

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const GEMINI_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Server-side fallback: on a policy decline the API retries the same request on
/// a fallback model inside the same call, so the island never shows a dead end.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 4096;
/// Text and code files are inlined; anything larger is skipped, as on macOS.
const MAX_INLINE_TEXT: u64 = 200_000;

pub const DEFAULT_MODEL: &str = "claude-opus-5";
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-2.0-flash";

const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen. \
You have web search access and can help with absolutely anything — research, coding, finding places, recommendations, tasks, questions. \
Respond in the user's language. Be thorough and complete — use as much detail as the task requires. \
No markdown formatting (no **, no ##, no bullet dashes). Use plain text with line breaks.";

pub fn system_prompt() -> String {
    let mem = crate::memory::format_memory_for_prompt();
    if mem.is_empty() {
        SYSTEM_PROMPT.to_string()
    } else {
        format!("{SYSTEM_PROMPT}\n\n{mem}")
    }
}

#[derive(Default)]
pub struct Chat {
    /// Full multi-turn history, including tool_use / tool_result blocks.
    messages: Mutex<Vec<Value>>,
}

impl Chat {
    pub fn reset(&self) {
        self.messages.lock().unwrap().clear();
    }

    pub fn load_from_history(&self, entries: &[crate::memory::ChatMessageEntry]) {
        let mut msgs = self.messages.lock().unwrap();
        msgs.clear();
        for entry in entries {
            msgs.push(json!({
                "role": entry.role,
                "content": entry.content
            }));
        }
    }

    fn is_empty(&self) -> bool {
        self.messages.lock().unwrap().is_empty()
    }

    fn push(&self, message: Value) {
        self.messages.lock().unwrap().push(message);
    }

    fn pop(&self) {
        self.messages.lock().unwrap().pop();
    }

    fn snapshot(&self) -> Vec<Value> {
        self.messages.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatContext {
    File { name: String, path: String },
    Window { app_name: String, title: String, url: Option<String> },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub text: String,
}

/// One chat turn. Dispatches to Gemini if model starts with "gemini" or if
/// only Gemini key is configured.
pub async fn send(
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let has_anthropic = secrets::present("anthropic-api-key");
    let has_gemini = secrets::present("gemini-api-key");

    if model.starts_with("gemini") {
        if has_gemini {
            send_gemini(chat, model, query, context).await
        } else if has_anthropic {
            send_anthropic(chat, "claude-opus-5", query, context).await
        } else {
            Err("Gemini API key missing. Open settings to enter your Google AI key.".to_string())
        }
    } else {
        if has_anthropic {
            send_anthropic(chat, model, query, context).await
        } else if has_gemini {
            send_gemini(chat, DEFAULT_GEMINI_MODEL, query, context).await
        } else {
            Err("API key missing. Open settings to enter your Anthropic or Google Gemini API key.".to_string())
        }
    }
}

fn parse_gemini_error(status: reqwest::StatusCode, text: &str) -> String {
    if let Ok(val) = serde_json::from_str::<Value>(text) {
        let obj = if let Some(arr) = val.as_array() {
            arr.first().unwrap_or(&val)
        } else {
            &val
        };

        if let Some(err) = obj.get("error") {
            let msg = err.get("message").and_then(Value::as_str);
            let status_code = err.get("code").and_then(Value::as_i64);
            let status_str = err.get("status").and_then(Value::as_str);

            if let Some(msg) = msg {
                let code_display = status_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| status.as_u16().to_string());

                if status == reqwest::StatusCode::FORBIDDEN || status_str == Some("PERMISSION_DENIED") {
                    if msg.contains("denied access") {
                        return format!(
                            "Gemini 403 (Permission Denied): Your Google AI key does not have access to this model (preview/billing required). Try selecting Gemini 2.0 Flash or 1.5 Flash in Settings."
                        );
                    }
                    return format!("Gemini 403 (Forbidden): {msg}");
                }

                if status == reqwest::StatusCode::NOT_FOUND || status_str == Some("NOT_FOUND") {
                    return format!(
                        "Gemini 404 (Not Found): The model was not found for this API key. Try selecting Gemini 2.0 Flash or 1.5 Flash in Settings."
                    );
                }

                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status_str == Some("RESOURCE_EXHAUSTED") {
                    return "Gemini 429 (Rate Limit): Quota exhausted. Please wait a moment before sending another prompt.".to_string();
                }

                if status == reqwest::StatusCode::UNAUTHORIZED || (status == reqwest::StatusCode::BAD_REQUEST && msg.to_lowercase().contains("api key")) {
                    return format!(
                        "Gemini API Key Error ({code_display}): {msg}. Please check your key in Settings."
                    );
                }

                return format!("Gemini API {code_display}: {msg}");
            }
        }
    }

    let snippet: String = text.chars().take(200).collect();
    if snippet.is_empty() {
        format!("Gemini API error {status} (empty response from server)")
    } else {
        format!("Gemini API {status}: {snippet}")
    }
}

async fn send_gemini(
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let key = secrets::get("gemini-api-key")
        .ok_or_else(|| "Gemini API key missing. Open settings.".to_string())?;

    let mut user_text = query.clone();
    let mut image_payload: Option<(String, String)> = None;

    if let Some(ctx) = &context {
        match ctx {
            ChatContext::File { name, path } => {
                match crate::files::inspect_file(path) {
                    crate::files::FileContentInfo::Text(text) => {
                        user_text = format!("File: {name}\nContents:\n{text}\n\n{user_text}");
                    }
                    crate::files::FileContentInfo::Docx(text) => {
                        user_text = format!("Word Document ({name}):\n{text}\n\n{user_text}");
                    }
                    crate::files::FileContentInfo::Pdf { text_preview, .. } => {
                        if !text_preview.is_empty() {
                            user_text = format!("PDF Document ({name}) Extracted Content:\n{text_preview}\n\n{user_text}");
                        } else {
                            user_text = format!("PDF Document ({name}) attached.\n\n{user_text}");
                        }
                    }
                    crate::files::FileContentInfo::Image { media_type, base64 } => {
                        image_payload = Some((media_type, base64));
                    }
                }
            }
            ChatContext::Window { app_name, title, url } => {
                let mut extra = format!("Context — App: {app_name}, Window: {title}");
                if let Some(u) = url {
                    extra.push_str(&format!(", URL: {u}"));
                }
                user_text = format!("{extra}\n\n{user_text}");
            }
        }
    }

    if let Some((media_type, base64)) = image_payload {
        chat.push(json!({
            "role": "user",
            "content": [
                { "type": "text", "text": user_text },
                { "type": "image_url", "image_url": { "url": format!("data:{media_type};base64,{base64}") } }
            ]
        }));
    } else {
        chat.push(json!({ "role": "user", "content": user_text }));
    }

    let sys_prompt = system_prompt();
    let mut messages = vec![json!({ "role": "system", "content": sys_prompt })];
    let snapshot = chat.snapshot();
    // Sliding context window: keep the last 12 messages for immediate conversational context
    // while preventing token explosion and keeping local CPU/network overhead minimal.
    let window_start = if snapshot.len() > 12 { snapshot.len() - 12 } else { 0 };
    for m in &snapshot[window_start..] {
        if let (Some(role), Some(content)) = (m.get("role"), m.get("content")) {
            if content.is_array() {
                messages.push(json!({ "role": role, "content": content }));
            } else if let Some(s) = content.as_str() {
                messages.push(json!({ "role": role, "content": s }));
            }
        }
    }

    let body = json!({
        "model": model,
        "messages": messages,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .post(GEMINI_ENDPOINT)
        .header("Authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            chat.pop();
            if e.is_timeout() {
                "Request timed out (90s): Gemini took too long to respond. The model or connection may be stalled.".to_string()
            } else if e.is_connect() {
                format!("Connection error: Could not reach Google AI server ({e})")
            } else {
                format!("Network error: {e}")
            }
        })?;

    let status = response.status();
    let text = response.text().await.map_err(|e| {
        chat.pop();
        format!("Could not read response: {e}")
    })?;
    if !status.is_success() {
        chat.pop();
        return Err(parse_gemini_error(status, &text));
    }

    let parsed: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            chat.pop();
            return Err(format!("Bad API response from Gemini: {e}"));
        }
    };

    let first_choice = parsed.get("choices").and_then(|c| c.get(0));

    // 1. Try standard text content
    let mut reply_text = first_choice
        .and_then(|c0| c0.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    // 2. If empty, check reasoning_content (used by thinking models)
    if reply_text.is_empty() {
        if let Some(reasoning) = first_choice
            .and_then(|c0| c0.get("message"))
            .and_then(|m| m.get("reasoning_content"))
            .and_then(Value::as_str)
        {
            reply_text = reasoning.trim().to_string();
        }
    }

    // 3. If still empty, inspect the exact reason so it never fails silently
    if reply_text.is_empty() {
        chat.pop();

        if let Some(refusal) = first_choice
            .and_then(|c0| c0.get("message"))
            .and_then(|m| m.get("refusal"))
            .and_then(Value::as_str)
        {
            return Err(format!("Gemini declined to answer: {refusal}"));
        }

        if let Some(finish_reason) = first_choice
            .and_then(|c0| c0.get("finish_reason"))
            .and_then(Value::as_str)
        {
            match finish_reason.to_lowercase().as_str() {
                "safety" | "content_filter" => {
                    return Err("Gemini blocked the response due to its content safety filters.".into());
                }
                "length" => {
                    return Err("Gemini reached its maximum output token limit before completing the response.".into());
                }
                "recitation" => {
                    return Err("Gemini blocked the response due to copyright/recitation policy.".into());
                }
                "stop" => {
                    return Err("Gemini completed generation without producing any text.".into());
                }
                other => {
                    return Err(format!("Gemini produced no text (finish reason: {other})."));
                }
            }
        }

        if let Some(block_reason) = parsed
            .get("promptFeedback")
            .and_then(|pf| pf.get("blockReason"))
            .and_then(Value::as_str)
        {
            return Err(format!("Gemini blocked the prompt: {block_reason}."));
        }

        return Err("Gemini returned an empty response with no text content.".into());
    }

    let cleaned_reply = crate::memory::extract_and_save_memory(&reply_text);

    chat.push(json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": cleaned_reply.clone() }]
    }));

    let _ = crate::memory::append_chat_turn(&query, &cleaned_reply);

    Ok(ChatReply { text: cleaned_reply })
}

async fn send_anthropic(
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let key = secrets::get("anthropic-api-key")
        .ok_or_else(|| "API key missing. Open settings.".to_string())?;

    let mut content: Vec<Value> = Vec::new();

    if let Some(ctx) = &context {
        match ctx {
            ChatContext::File { name, path } => {
                match crate::files::inspect_file(path) {
                    crate::files::FileContentInfo::Text(text) => {
                        content.push(json!({
                            "type": "text",
                            "text": format!("File: {name}\nContents:\n{text}")
                        }));
                    }
                    crate::files::FileContentInfo::Docx(text) => {
                        content.push(json!({
                            "type": "text",
                            "text": format!("Word Document ({name}):\n{text}")
                        }));
                    }
                    crate::files::FileContentInfo::Pdf { base64, .. } => {
                        content.push(json!({
                            "type": "document",
                            "source": { "type": "base64", "media_type": "application/pdf", "data": base64 }
                        }));
                        content.push(json!({ "type": "text", "text": format!("File: {name}") }));
                    }
                    crate::files::FileContentInfo::Image { media_type, base64 } => {
                        content.push(json!({
                            "type": "image",
                            "source": { "type": "base64", "media_type": media_type, "data": base64 }
                        }));
                        content.push(json!({ "type": "text", "text": format!("File: {name}") }));
                    }
                }
            }
            ChatContext::Window { app_name, title, url } => {
                let mut text = format!("Context — App: {app_name}, Window: {title}");
                if let Some(url) = url {
                    text.push_str(&format!(", URL: {url}"));
                }
                content.push(json!({ "type": "text", "text": text }));
            }
        }
    }
    content.push(json!({ "type": "text", "text": query.clone() }));

    chat.push(json!({ "role": "user", "content": content }));

    let snapshot = chat.snapshot();
    let window_start = if snapshot.len() > 12 { snapshot.len() - 12 } else { 0 };
    let window_messages: Vec<Value> = snapshot[window_start..].to_vec();

    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system_prompt(),
        "tools": [{ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 }],
        "fallbacks": "default",
        "messages": window_messages,
    });

    let response = match call(&key, &body).await {
        Ok(v) => v,
        Err(err) => {
            chat.pop(); // keep the history consistent with what the model saw
            return Err(err);
        }
    };

    // A policy decline comes back as HTTP 200 with stop_reason "refusal".
    if response.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        chat.pop();
        let why = response
            .get("stop_details")
            .and_then(|d| d.get("explanation"))
            .and_then(Value::as_str)
            .unwrap_or("Claude declined this request due to policy.");
        return Err(why.to_string());
    }

    let Some(blocks) = response.get("content").and_then(Value::as_array).cloned() else {
        chat.pop();
        return Err("Unexpected API response from Claude (missing content array).".into());
    };

    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if text.is_empty() {
        chat.pop();
        if let Some(stop) = response.get("stop_reason").and_then(Value::as_str) {
            match stop {
                "max_tokens" => {
                    return Err("Claude reached its maximum output token limit before generating text.".into());
                }
                "tool_use" => {
                    return Err("Claude requested a tool action without generating text.".into());
                }
                other => {
                    return Err(format!("Claude finished without text (stop reason: {other})."));
                }
            }
        }
        return Err("Claude returned an empty response.".into());
    }

    let cleaned_reply = crate::memory::extract_and_save_memory(&text);

    // Store the whole content — tool_use / tool_result blocks included — so the
    // next turn has the right context.
    chat.push(json!({ "role": "assistant", "content": blocks.clone() }));
    let _ = crate::memory::append_chat_turn(&query, &cleaned_reply);
    Ok(ChatReply { text: cleaned_reply })
}

async fn call(key: &str, body: &Value) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .post(ENDPOINT)
        .header("x-api-key", key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", FALLBACK_BETA)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "Request timed out (90s): Claude took too long to respond. The server or connection may be stalled.".to_string()
            } else if e.is_connect() {
                format!("Connection error: Could not reach Anthropic server ({e})")
            } else {
                format!("Network error: {e}")
            }
        })?;

    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.chars().take(200).collect());

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(format!("Claude API Key Invalid (401): {detail}"));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err("Claude Rate Limit (429): Quota or rate limit exceeded. Please wait a moment.".into());
        }
        return Err(format!("Claude API {status}: {detail}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("Bad API response from Claude: {e}"))
}



/// Small standalone base64 encoder — not worth another dependency.
/// Also used for Stripe's basic auth.
pub(crate) fn base64_for(bytes: &[u8]) -> String {
    base64(bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
