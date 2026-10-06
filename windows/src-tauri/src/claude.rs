// Claude API client — the same integration as ClaudeService.swift: multi-turn
// chat with web search, and files sent as document/image/text blocks.
//
// Everything happens here rather than in the island: the API key never leaves
// the Credential Manager, and file bytes never cross the IPC boundary.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::secrets;

static CHAT_CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn cancel_current_turn() {
    CHAT_CANCELLED.store(true, Ordering::SeqCst);
}

pub fn is_turn_cancelled() -> bool {
    CHAT_CANCELLED.load(Ordering::SeqCst)
}

pub fn reset_turn_cancelled() {
    CHAT_CANCELLED.store(false, Ordering::SeqCst);
}

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

const SYSTEM_PROMPT: &str = "You are Mochi, a personal AI assistant living at the top of the user's screen in Coucou.";

fn day_of_week(y: u32, m: u32, d: u32) -> &'static str {
    let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y.saturating_sub(1) } else { y };
    let dow = (y + y / 4 - y / 100 + y / 400 + t[(m.saturating_sub(1) % 12) as usize] + d) % 7;
    match dow {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        _ => "Saturday",
    }
}

fn month_name(m: u32) -> &'static str {
    match m {
        1 => "January", 2 => "February", 3 => "March", 4 => "April",
        5 => "May", 6 => "June", 7 => "July", 8 => "August",
        9 => "September", 10 => "October", 11 => "November", _ => "December",
    }
}

pub fn system_prompt() -> String {
    let lt = crate::platform::local_time();
    let dow = day_of_week(lt.year, lt.month, lt.day);
    let month = month_name(lt.month);
    let date_str = format!("{dow}, {month} {}, {}", lt.day, lt.year);
    let time_str = format!("{:02}:{:02}", lt.hour, lt.minute);
    let mem = crate::memory::format_memory_for_prompt();

    let temporal_ctx = format!(
        "You are Mochi, a warm, helpful, and concise AI companion living on the user's screen in Coucou.\n\
         Current date: {}.\n\
         Current time: {} (local PC clock).\n\
         User location: Nairobi, Kenya (Timezone: EAT / UTC+3).\n\
         Current year: {}.\n\n\
         Core instructions:\n\
         • Speak naturally and directly as Mochi. Never refer to yourself as 'the system', never say 'in the process of being confirmed' or 'waiting for confirmation', and never mention internal system processing.\n\
         • Today is in October 2026. The upcoming NBA season is the 2026-2027 season (starting October 2026). The 2025-2026 NBA season ended in June 2026.\n\
         • You have live web search capabilities. When live search findings are provided below, answer directly using those facts.\n\
         • Plain text only (no markdown *, #, or bullets). Keep responses crisp, natural, and helpful.\n\
         • Long-term memory: When the user shares personal details (name, preferences, interests, favorite teams) or asks you to remember something, append at the end:\n\
           <remember category=\"preference\">fact to remember</remember>",
        date_str, time_str, lt.year
    );

    if mem.is_empty() {
        temporal_ctx
    } else {
        format!("{temporal_ctx}\n\nRemembered facts about user:\n{mem}")
    }
}

// ── Web Search & Grounding ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchItem {
    pub title: String,
    pub snippet: String,
}

fn urlencoding_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

fn strip_html_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out.replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

fn parse_lite_duckduckgo_html(html: &str) -> Vec<SearchItem> {
    let mut titles = Vec::new();
    let mut snippets = Vec::new();

    let mut search_from = 0;
    while let Some(pos) = html[search_from..].find("result-link") {
        let abs_pos = search_from + pos;
        if let Some(open_gt) = html[abs_pos..].find('>') {
            let title_start = abs_pos + open_gt + 1;
            if let Some(close_a) = html[title_start..].find("</a>") {
                let raw_title = &html[title_start..title_start + close_a];
                let title = strip_html_tags(raw_title);
                if !title.is_empty() {
                    titles.push(title);
                }
                search_from = title_start + close_a + 4;
                continue;
            }
        }
        search_from = abs_pos + 11;
    }

    let mut search_from = 0;
    while let Some(pos) = html[search_from..].find("result-snippet") {
        let abs_pos = search_from + pos;
        if let Some(open_gt) = html[abs_pos..].find('>') {
            let snip_start = abs_pos + open_gt + 1;
            if let Some(close_td) = html[snip_start..].find("</td>") {
                let raw_snip = &html[snip_start..snip_start + close_td];
                let snippet = strip_html_tags(raw_snip);
                if !snippet.is_empty() {
                    snippets.push(snippet);
                }
                search_from = snip_start + close_td + 5;
                continue;
            }
        }
        search_from = abs_pos + 14;
    }

    let mut items = Vec::new();
    let count = titles.len().min(snippets.len()).min(4);
    for i in 0..count {
        items.push(SearchItem {
            title: titles[i].clone(),
            snippet: snippets[i].clone(),
        });
    }
    if items.is_empty() && !snippets.is_empty() {
        for s in snippets.into_iter().take(4) {
            items.push(SearchItem {
                title: "Web Source".to_string(),
                snippet: s,
            });
        }
    }
    items
}

static SEARCH_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn search_client() -> &'static reqwest::Client {
    SEARCH_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(6))
            .pool_max_idle_per_host(4)
            .build()
            .unwrap_or_default()
    })
}

pub async fn perform_web_search(query: &str) -> Vec<SearchItem> {
    let client = search_client();

    let params = [("q", query)];
    let resp = client
        .post("https://lite.duckduckgo.com/lite/")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&params)
        .send()
        .await;

    if let Ok(r) = resp {
        if let Ok(html) = r.text().await {
            let items = parse_lite_duckduckgo_html(&html);
            if !items.is_empty() {
                return items;
            }
        }
    }

    let enc = urlencoding_encode(query);
    let api_url = format!("https://api.duckduckgo.com/?q={enc}&format=json");
    if let Ok(r) = client.get(&api_url).header("User-Agent", "Mozilla/5.0").send().await {
        if let Ok(val) = r.json::<Value>().await {
            let mut fallback_items = Vec::new();
            if let Some(abs) = val.get("AbstractText").and_then(Value::as_str) {
                if !abs.is_empty() {
                    let heading = val.get("Heading").and_then(Value::as_str).unwrap_or("Web Result");
                    fallback_items.push(SearchItem {
                        title: heading.to_string(),
                        snippet: abs.to_string(),
                    });
                }
            }
            if !fallback_items.is_empty() {
                return fallback_items;
            }
        }
    }

    Vec::new()
}

pub fn format_grounding_system_context(search_query: &str, search_items: &[SearchItem]) -> String {
    let mut block = String::from("\n\nLive web search findings:\n");
    for (i, item) in search_items.iter().enumerate() {
        block.push_str(&format!(
            "[{}] {}: {}\n",
            i + 1, item.title.trim(), item.snippet.trim()
        ));
    }
    block.push_str(
        "\nInstruction: Answer the user's question directly using the search findings above. Speak naturally as Mochi. Never say 'the system performed a search', never say 'waiting for confirmation', and never output search metadata. Just answer directly in plain conversational text."
    );
    block
}

pub fn should_search_web(query: &str) -> bool {
    let q = query.to_lowercase();
    if q.contains("search")
        || q.contains("google")
        || q.contains("look up")
        || q.contains("browse")
        || q.contains("internet")
        || q.contains("online")
        || q.contains("find out")
        || q.contains("check the web")
    {
        return true;
    }
    if q.contains("current")
        || q.contains("latest")
        || q.contains("today")
        || q.contains("tonight")
        || q.contains("now")
        || q.contains("recent")
        || q.contains("this week")
        || q.contains("this month")
        || q.contains("this year")
    {
        return true;
    }
    if q.contains("season")
        || q.contains("started")
        || q.contains("starts")
        || q.contains("start")
        || q.contains("who won")
        || q.contains("winner")
        || q.contains("champion")
        || q.contains("score")
        || q.contains("standing")
        || q.contains("schedule")
        || q.contains("nba")
        || q.contains("epl")
        || q.contains("premier league")
        || q.contains("champions league")
        || q.contains("game")
        || q.contains("match")
    {
        return true;
    }
    if q.contains("2024") || q.contains("2025") || q.contains("2026") || q.contains("2027") {
        return true;
    }
    if q.starts_with("has ")
        || q.starts_with("did ")
        || q.starts_with("is ")
        || q.starts_with("when is")
        || q.starts_with("when does")
        || q.starts_with("who is the current")
        || q.starts_with("what is the current")
        || q.starts_with("what's the current")
    {
        return true;
    }
    false
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
    pub memory_updated: bool,
}

/// One chat turn. Dispatches to Gemini if model starts with "gemini" or if
/// only Gemini key is configured.
pub async fn send(
    app: &AppHandle,
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    reset_turn_cancelled();

    if model.starts_with("local:") || model.starts_with("ollama:") || model.starts_with("lmstudio:") {
        let clean_model = model
            .strip_prefix("local:")
            .or_else(|| model.strip_prefix("ollama:"))
            .or_else(|| model.strip_prefix("lmstudio:"))
            .unwrap_or(model);
        return send_local(app, chat, clean_model, query, context).await;
    }

    let has_anthropic = secrets::present("anthropic-api-key");
    let has_gemini = secrets::present("gemini-api-key");

    if model.starts_with("gemini") {
        if has_gemini {
            send_gemini(app, chat, model, query, context).await
        } else if has_anthropic {
            send_anthropic(app, chat, "claude-opus-5", query, context).await
        } else {
            Err("Gemini API key missing. Open settings to enter your Google AI key.".to_string())
        }
    } else {
        if has_anthropic {
            send_anthropic(app, chat, model, query, context).await
        } else if has_gemini {
            send_gemini(app, chat, DEFAULT_GEMINI_MODEL, query, context).await
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
    app: &AppHandle,
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let key = secrets::get("gemini-api-key")
        .ok_or_else(|| "Gemini API key missing. Open settings.".to_string())?;

    let mut user_text = query.clone();
    let mut image_payload: Option<(String, String)> = None;

    let mut grounding_block: Option<String> = None;

    if should_search_web(&query) {
        let _ = app.emit("chat-status", json!({ "status": "searching", "detail": "Searching the web…", "siteCount": 0 }));
        let search_query = if !query.contains("202") && (query.to_lowercase().contains("season") || query.to_lowercase().contains("nba") || query.to_lowercase().contains("winner") || query.to_lowercase().contains("champion") || query.to_lowercase().contains("score")) {
            format!("{query} 2026")
        } else {
            query.clone()
        };
        let search_items = perform_web_search(&search_query).await;
        if is_turn_cancelled() {
            return Ok(ChatReply {
                text: String::new(),
                memory_updated: false,
            });
        }
        if !search_items.is_empty() {
            let _ = app.emit("chat-status", json!({ "status": "searched", "detail": format!("Searched {} websites", search_items.len()), "siteCount": search_items.len() }));
            grounding_block = Some(format_grounding_system_context(&query, &search_items));
        }
    }

    if is_turn_cancelled() {
        return Ok(ChatReply {
            text: String::new(),
            memory_updated: false,
        });
    }

    let _ = app.emit("chat-status", json!({ "status": "thinking", "detail": "Thinking…" }));

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

    let mut sys_prompt = system_prompt();
    if let Some(g) = grounding_block {
        sys_prompt.push_str(&g);
    }
    let mut messages = vec![json!({ "role": "system", "content": sys_prompt })];
    let snapshot = chat.snapshot();
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
        "stream": true,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let mut response = client
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

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        chat.pop();
        return Err(parse_gemini_error(status, &text));
    }

    let mut reply_text = String::new();
    let mut buffer = String::new();

    while let Ok(Some(chunk)) = response.chunk().await {
        if is_turn_cancelled() {
            break;
        }
        let chunk_str = String::from_utf8_lossy(&chunk);
        buffer.push_str(&chunk_str);
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();
            if let Some(json_str) = line.strip_prefix("data: ") {
                let json_trimmed = json_str.trim();
                if json_trimmed == "[DONE]" {
                    break;
                }
                if let Ok(v) = serde_json::from_str::<Value>(json_trimmed) {
                    if let Some(delta) = v.get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c0| c0.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(Value::as_str)
                    {
                        if !delta.is_empty() {
                            reply_text.push_str(delta);
                            let _ = app.emit("chat-token", json!({ "token": delta, "done": false }));
                        }
                    }
                }
            }
        }
    }

    let _ = app.emit("chat-token", json!({ "token": "", "done": true }));

    if is_turn_cancelled() {
        if reply_text.trim().is_empty() {
            chat.pop();
            return Ok(ChatReply {
                text: String::new(),
                memory_updated: false,
            });
        }
    } else if reply_text.trim().is_empty() {
        chat.pop();
        return Err("Gemini returned an empty response.".into());
    }

    let (cleaned_reply, memory_updated) = crate::memory::extract_and_save_memory(&query, &reply_text);
    if memory_updated {
        let _ = app.emit("memory-updated", json!({ "updated": true }));
    }

    chat.push(json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": cleaned_reply.clone() }]
    }));

    let _ = crate::memory::append_chat_turn(&query, &cleaned_reply);

    Ok(ChatReply {
        text: cleaned_reply,
        memory_updated,
    })
}

static LLAMA_SERVER_CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);

pub fn stop_local_engine() {
    let mut lock = LLAMA_SERVER_CHILD.lock().unwrap();
    if let Some(mut child) = lock.take() {
        let _ = child.kill();
    }
}

fn scan_ollama_manifests(dir: &std::path::Path, out: &mut Vec<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_ollama_manifests(&path, out);
            } else if path.is_file() {
                if let (Some(tag), Some(model)) = (
                    path.file_name().and_then(|f| f.to_str()),
                    path.parent().and_then(|p| p.file_name()).and_then(|f| f.to_str()),
                ) {
                    let formatted = if tag == "latest" {
                        format!("ollama:{model}")
                    } else {
                        format!("ollama:{model}:{tag}")
                    };
                    if !out.contains(&formatted) {
                        out.push(formatted);
                    }
                }
            }
        }
    }
}

pub async fn test_local_server(custom_url: Option<&str>) -> Result<Vec<String>, String> {
    let mut found = Vec::new();

    // 1. Scan Coucou Native GGUF models in %LOCALAPPDATA%\Coucou\models
    let models_dir = crate::settings::models_dir();
    if let Ok(entries) = std::fs::read_dir(&models_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().map(|e| e == "gguf").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    found.push(format!("coucou:{stem}"));
                }
            }
        }
    }

    // 2. Query Ollama /api/tags if running or accessible
    let endpoint = match custom_url {
        Some(u) if !u.trim().is_empty() => u.trim().to_string(),
        _ => secrets::get("local-model-url").unwrap_or_else(|| "http://127.0.0.1:11434".to_string()),
    };
    let endpoint = endpoint.trim_end_matches('/');

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .build()
        .map_err(|e| e.to_string())?;

    let tags_url = format!("{endpoint}/api/tags");
    if let Ok(res) = client.get(&tags_url).send().await {
        if res.status().is_success() {
            if let Ok(json) = res.json::<Value>().await {
                if let Some(models) = json.get("models").and_then(Value::as_array) {
                    for m in models {
                        if let Some(name) = m.get("name").and_then(Value::as_str) {
                            let tag = format!("ollama:{name}");
                            if !found.contains(&tag) {
                                found.push(tag);
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. If Ollama service is stopped, detect any models stored on disk in %USERPROFILE%\.ollama
    let user_home = crate::platform::home_dir();
    let ollama_manifests = user_home.join(".ollama").join("models").join("manifests");
    if ollama_manifests.exists() {
        scan_ollama_manifests(&ollama_manifests, &mut found);
    }

    // 4. Try OpenAI-compatible /v1/models (LM Studio, LocalAI, vLLM)
    let models_url = format!("{endpoint}/v1/models");
    if let Ok(res) = client.get(&models_url).send().await {
        if res.status().is_success() {
            if let Ok(json) = res.json::<Value>().await {
                if let Some(data) = json.get("data").and_then(Value::as_array) {
                    for item in data {
                        if let Some(id) = item.get("id").and_then(Value::as_str) {
                            let tag = format!("openai:{id}");
                            if !found.contains(&tag) {
                                found.push(tag);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(found)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgressPayload {
    pub kind: String,        // "engine" | "model"
    pub status: String,
    pub completed: u64,
    pub total: u64,
    pub percent: f64,
    pub done: bool,
    pub error: Option<String>,
}

pub async fn install_engine(app: &tauri::AppHandle) -> Result<String, String> {
    use tauri::Emitter;

    let server_path = crate::settings::llama_server_path();
    if server_path.exists() {
        let _ = app.emit("local-download-progress", DownloadProgressPayload {
            kind: "engine".to_string(),
            status: "Coucou Native Engine (llama-server) is already installed and ready!".to_string(),
            completed: 100,
            total: 100,
            percent: 100.0,
            done: true,
            error: None,
        });
        return Ok("Coucou Native Engine (llama-server) is already installed!".to_string());
    }

    let _ = app.emit("local-download-progress", DownloadProgressPayload {
        kind: "engine".to_string(),
        status: "Connecting to download Coucou Native Engine (18.5 MB)…".to_string(),
        completed: 0,
        total: 19_399_521,
        percent: 0.0,
        done: false,
        error: None,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;

    // Official llama.cpp Windows AVX2 release (ultralight standalone CPU runner)
    let url = "https://github.com/ggml-org/llama.cpp/releases/download/b11433/llama-b11433-bin-win-cpu-x64.zip";
    let mut res = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach engine download URL: {e}"))?;

    if !res.status().is_success() {
        return Err(format!("Download failed with HTTP {}", res.status()));
    }

    let total_bytes = res.content_length().unwrap_or(19_399_521);
    let temp_zip = std::env::temp_dir().join("Coucou_llama_engine.zip");

    let mut file = std::fs::File::create(&temp_zip)
        .map_err(|e| format!("Could not create temporary file: {e}"))?;

    use std::io::Write;
    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();

    while let Some(chunk) = res.chunk().await.map_err(|e| format!("Download error: {e}"))? {
        file.write_all(&chunk).map_err(|e| format!("Failed to write: {e}"))?;
        downloaded += chunk.len() as u64;

        if last_emit.elapsed().as_millis() > 100 || downloaded >= total_bytes {
            last_emit = std::time::Instant::now();
            let percent = if total_bytes > 0 {
                ((downloaded as f64 / total_bytes as f64) * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            };
            let _ = app.emit("local-download-progress", DownloadProgressPayload {
                kind: "engine".to_string(),
                status: format!("Downloading Coucou Engine ({:.0}%)…", percent),
                completed: downloaded,
                total: total_bytes,
                percent,
                done: false,
                error: None,
            });
        }
    }
    drop(file);

    let _ = app.emit("local-download-progress", DownloadProgressPayload {
        kind: "engine".to_string(),
        status: "Extracting standalone engine into Coucou bin…".to_string(),
        completed: total_bytes,
        total: total_bytes,
        percent: 100.0,
        done: false,
        error: None,
    });

    let bin_dir = crate::settings::local_dir().join("bin");
    std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;

    let temp_zip_clone = temp_zip.clone();
    let bin_dir_clone = bin_dir.clone();
    let extract_res = tauri::async_runtime::spawn_blocking(move || {
        // Try bsdtar first
        let tar_out = std::process::Command::new("tar")
            .args(&["-xf", temp_zip_clone.to_string_lossy().as_ref(), "-C", bin_dir_clone.to_string_lossy().as_ref()])
            .output();

        if let Ok(ref out) = tar_out {
            if out.status.success() {
                let _ = std::fs::remove_file(&temp_zip_clone);
                return tar_out;
            }
        }

        // Fallback to PowerShell Expand-Archive
        let ps_cmd = format!(
            "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
            temp_zip_clone.to_string_lossy(),
            bin_dir_clone.to_string_lossy()
        );
        let ps_out = std::process::Command::new("powershell")
            .args(&["-NoProfile", "-Command", &ps_cmd])
            .output();

        let _ = std::fs::remove_file(&temp_zip_clone);
        ps_out
    }).await.map_err(|e| e.to_string())?;

    match extract_res {
        Ok(out) if out.status.success() => {
            let _ = app.emit("local-download-progress", DownloadProgressPayload {
                kind: "engine".to_string(),
                status: "Coucou Native Engine (llama-server) installed successfully!".to_string(),
                completed: total_bytes,
                total: total_bytes,
                percent: 100.0,
                done: true,
                error: None,
            });
            Ok("Coucou Native Engine (llama-server) installed successfully!".to_string())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(format!("Extraction error: {stderr}"))
        }
        Err(e) => Err(format!("Could not extract engine: {e}")),
    }
}

pub async fn pull_model(app: &tauri::AppHandle, model_name: String) -> Result<String, String> {
    use tauri::Emitter;

    let (filename, download_url, expected_size) = match model_name.to_lowercase().as_str() {
        m if m.contains("qwen") && m.contains("0.5") => (
            "qwen2.5-0.5b-instruct-q4_k_m.gguf",
            "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf",
            397_800_000u64,
        ),
        m if m.contains("3b") => (
            "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
            "https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf",
            2_020_000_000u64,
        ),
        _ => (
            "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
            "https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/resolve/main/Llama-3.2-1B-Instruct-Q4_K_M.gguf",
            807_694_464u64,
        ),
    };

    let models_dir = crate::settings::models_dir();
    std::fs::create_dir_all(&models_dir).map_err(|e| e.to_string())?;
    let dest_file = models_dir.join(filename);

    // CRITICAL: Prevent downloading twice!
    if dest_file.exists() {
        if let Ok(meta) = std::fs::metadata(&dest_file) {
            if meta.len() > 10_000_000 {
                let _ = app.emit("local-download-progress", DownloadProgressPayload {
                    kind: "model".to_string(),
                    status: format!("Model '{filename}' is already downloaded and ready!"),
                    completed: meta.len(),
                    total: meta.len(),
                    percent: 100.0,
                    done: true,
                    error: None,
                });
                return Ok(format!("Model '{filename}' is already downloaded in Coucou storage and ready!"));
            }
        }
    }

    let _ = app.emit("local-download-progress", DownloadProgressPayload {
        kind: "model".to_string(),
        status: format!("Connecting to download '{filename}'…"),
        completed: 0,
        total: expected_size,
        percent: 0.0,
        done: false,
        error: None,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()
        .map_err(|e| e.to_string())?;

    let mut res = client
        .get(download_url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach model download server: {e}"))?;

    if !res.status().is_success() {
        return Err(format!("Model download failed with HTTP {}", res.status()));
    }

    let total_bytes = res.content_length().unwrap_or(expected_size);
    let mut file = std::fs::File::create(&dest_file)
        .map_err(|e| format!("Could not create model destination file: {e}"))?;

    use std::io::Write;
    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();

    while let Some(chunk) = res.chunk().await.map_err(|e| format!("Stream error: {e}"))? {
        file.write_all(&chunk).map_err(|e| format!("Failed to write chunk: {e}"))?;
        downloaded += chunk.len() as u64;

        if last_emit.elapsed().as_millis() > 120 || downloaded >= total_bytes {
            last_emit = std::time::Instant::now();
            let percent = if total_bytes > 0 {
                ((downloaded as f64 / total_bytes as f64) * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            };
            let _ = app.emit("local-download-progress", DownloadProgressPayload {
                kind: "model".to_string(),
                status: format!("Downloading model weights ({:.0}%)…", percent),
                completed: downloaded,
                total: total_bytes,
                percent,
                done: false,
                error: None,
            });
        }
    }
    drop(file);

    let _ = app.emit("local-download-progress", DownloadProgressPayload {
        kind: "model".to_string(),
        status: format!("Model '{filename}' successfully downloaded and ready!"),
        completed: total_bytes,
        total: total_bytes,
        percent: 100.0,
        done: true,
        error: None,
    });

    Ok(format!("Model '{filename}' successfully downloaded and ready for Mochi!"))
}

async fn ensure_llama_server_running(model_tag: &str) -> Result<String, String> {
    let server_exe = crate::settings::llama_server_path();
    if !server_exe.exists() {
        return Err("Coucou Native Engine (llama-server) is not installed. Open Settings and click '⚡ Install Coucou Engine'.".to_string());
    }

    let models_dir = crate::settings::models_dir();
    let clean_tag = model_tag.strip_prefix("coucou:").unwrap_or(model_tag);
    let mut model_file = None;

    if let Ok(entries) = std::fs::read_dir(&models_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if stem.eq_ignore_ascii_case(clean_tag) || clean_tag.contains(stem) || stem.contains(clean_tag) {
                        model_file = Some(path);
                        break;
                    }
                }
            }
        }
    }

    let model_path = match model_file {
        Some(p) => p,
        None => {
            if let Ok(mut entries) = std::fs::read_dir(&models_dir) {
                if let Some(first) = entries.find_map(|e| e.ok().map(|e| e.path()).filter(|p| p.extension().map(|ext| ext == "gguf").unwrap_or(false))) {
                    first
                } else {
                    return Err(format!("Model '{clean_tag}' not found in Coucou models. Open Settings and click '⬇️ Download Model'."));
                }
            } else {
                return Err("No models found in Coucou models directory. Open Settings to download one.".to_string());
            }
        }
    };

    // Check if server is already responding
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_millis(600)).build().unwrap_or_default();
    if client.get("http://127.0.0.1:8080/health").send().await.map(|r| r.status().is_success()).unwrap_or(false) {
        return Ok("http://127.0.0.1:8080".to_string());
    }

    // Stop previous instance if any
    stop_local_engine();

    // Spawn llama-server with strict thermal protection (-t 2) and CPU mode (--n-gpu-layers 0)
    let mut cmd = std::process::Command::new(&server_exe);
    cmd.args(&[
        "-m", model_path.to_string_lossy().as_ref(),
        "--port", "8080",
        "-t", "2",
        "-c", "4096",
        "--n-gpu-layers", "0",
    ]);
    #[cfg(windows)]
    crate::platform::no_console(&mut cmd);

    let child = cmd.spawn().map_err(|e| format!("Failed to spawn llama-server: {e}"))?;
    {
        let mut lock = LLAMA_SERVER_CHILD.lock().unwrap();
        *lock = Some(child);
    }

    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < 5 {
        tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
        if client.get("http://127.0.0.1:8080/health").send().await.map(|r| r.status().is_success()).unwrap_or(false) {
            return Ok("http://127.0.0.1:8080".to_string());
        }
    }

    Ok("http://127.0.0.1:8080".to_string())
}

async fn send_local(
    app: &AppHandle,
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let (endpoint, actual_model) = if model.starts_with("ollama:") {
        let ep = secrets::get("local-model-url")
            .unwrap_or_else(|| "http://127.0.0.1:11434".to_string());
        (ep, model.strip_prefix("ollama:").unwrap_or(model).to_string())
    } else {
        let ep = ensure_llama_server_running(model).await?;
        (ep, model.strip_prefix("coucou:").unwrap_or(model).to_string())
    };
    let endpoint = endpoint.trim_end_matches('/');

    let mut user_text = query.clone();
    let mut grounding_block: Option<String> = None;

    if should_search_web(&query) {
        let _ = app.emit("chat-status", json!({ "status": "searching", "detail": "Searching the web…", "siteCount": 0 }));
        let search_query = if !query.contains("202") && (query.to_lowercase().contains("season") || query.to_lowercase().contains("nba") || query.to_lowercase().contains("winner") || query.to_lowercase().contains("champion") || query.to_lowercase().contains("score")) {
            format!("{query} 2026")
        } else {
            query.clone()
        };
        let search_items = perform_web_search(&search_query).await;
        if is_turn_cancelled() {
            return Ok(ChatReply {
                text: String::new(),
                memory_updated: false,
            });
        }
        if !search_items.is_empty() {
            let _ = app.emit("chat-status", json!({ "status": "searched", "detail": format!("Searched {} websites", search_items.len()), "siteCount": search_items.len() }));
            grounding_block = Some(format_grounding_system_context(&query, &search_items));
        }
    }

    if is_turn_cancelled() {
        return Ok(ChatReply {
            text: String::new(),
            memory_updated: false,
        });
    }

    let _ = app.emit("chat-status", json!({ "status": "thinking", "detail": "Thinking…" }));

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
                    crate::files::FileContentInfo::Image { .. } => {
                        user_text = format!("Image ({name}) attached.\n\n{user_text}");
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

    chat.push(json!({ "role": "user", "content": user_text }));

    let mut sys_prompt = system_prompt();
    if let Some(g) = grounding_block {
        sys_prompt.push_str(&g);
    }
    let mut messages = vec![json!({ "role": "system", "content": sys_prompt })];
    let snapshot = chat.snapshot();
    let window_start = if snapshot.len() > 10 { snapshot.len() - 10 } else { 0 };
    for m in &snapshot[window_start..] {
        if let (Some(role), Some(content)) = (m.get("role"), m.get("content")) {
            if let Some(s) = content.as_str() {
                messages.push(json!({ "role": role, "content": s }));
            } else if let Some(arr) = content.as_array() {
                let text = arr
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    messages.push(json!({ "role": role, "content": text }));
                }
            }
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let mut reply_text = String::new();

    // Attempt 1: OpenAI-compatible /v1/chat/completions (supported by llama-server, Ollama, LM Studio)
    let body = json!({
        "model": actual_model,
        "messages": messages,
        "temperature": 0.7,
        "stream": true,
    });

    let openai_url = format!("{endpoint}/v1/chat/completions");
    let res = client
        .post(&openai_url)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await;

    match res {
        Ok(mut resp) if resp.status().is_success() => {
            let mut buffer = String::new();
            while let Ok(Some(chunk)) = resp.chunk().await {
                if is_turn_cancelled() {
                    break;
                }
                let chunk_str = String::from_utf8_lossy(&chunk);
                buffer.push_str(&chunk_str);
                while let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].trim().to_string();
                    buffer = buffer[pos + 1..].to_string();
                    if let Some(json_str) = line.strip_prefix("data: ") {
                        let json_trimmed = json_str.trim();
                        if json_trimmed == "[DONE]" {
                            break;
                        }
                        if let Ok(v) = serde_json::from_str::<Value>(json_trimmed) {
                            if let Some(delta) = v.get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c0| c0.get("delta"))
                                .and_then(|d| d.get("content"))
                                .and_then(Value::as_str)
                            {
                                if !delta.is_empty() {
                                    reply_text.push_str(delta);
                                    let _ = app.emit("chat-token", json!({ "token": delta, "done": false }));
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
            // Attempt 2: Fallback to native Ollama /api/chat
            let ollama_url = format!("{endpoint}/api/chat");
            let ollama_body = json!({
                "model": model,
                "messages": messages,
                "stream": true,
            });
            if let Ok(mut o_res) = client
                .post(&ollama_url)
                .header("content-type", "application/json")
                .json(&ollama_body)
                .send()
                .await
            {
                let mut buffer = String::new();
                while let Ok(Some(chunk)) = o_res.chunk().await {
                    if is_turn_cancelled() {
                        break;
                    }
                    let chunk_str = String::from_utf8_lossy(&chunk);
                    buffer.push_str(&chunk_str);
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].trim().to_string();
                        buffer = buffer[pos + 1..].to_string();
                        if !line.is_empty() {
                            if let Ok(val) = serde_json::from_str::<Value>(&line) {
                                if let Some(content) = val.get("message").and_then(|m| m.get("content")).and_then(Value::as_str) {
                                    if !content.is_empty() {
                                        reply_text.push_str(content);
                                        let _ = app.emit("chat-token", json!({ "token": content, "done": false }));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }

    let _ = app.emit("chat-token", json!({ "token": "", "done": true }));

    if is_turn_cancelled() {
        if reply_text.trim().is_empty() {
            chat.pop();
            return Ok(ChatReply {
                text: String::new(),
                memory_updated: false,
            });
        }
    } else if reply_text.trim().is_empty() {
        chat.pop();
        return Err("Local model produced an empty response.".into());
    }

    let (cleaned_reply, memory_updated) = crate::memory::extract_and_save_memory(&query, &reply_text);
    if memory_updated {
        let _ = app.emit("memory-updated", json!({ "updated": true }));
    }

    chat.push(json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": cleaned_reply.clone() }]
    }));
    let _ = crate::memory::append_chat_turn(&query, &cleaned_reply);

    Ok(ChatReply {
        text: cleaned_reply,
        memory_updated,
    })
}

async fn send_anthropic(
    app: &AppHandle,
    chat: &Chat,
    model: &str,
    query: String,
    context: Option<ChatContext>,
) -> Result<ChatReply, String> {
    let key = secrets::get("anthropic-api-key")
        .ok_or_else(|| "API key missing. Open settings.".to_string())?;

    let mut user_text = query.clone();
    let mut grounding_block: Option<String> = None;

    if should_search_web(&query) {
        let _ = app.emit("chat-status", json!({ "status": "searching", "detail": "Searching the web…", "siteCount": 0 }));
        let search_query = if !query.contains("202") && (query.to_lowercase().contains("season") || query.to_lowercase().contains("nba") || query.to_lowercase().contains("winner") || query.to_lowercase().contains("champion") || query.to_lowercase().contains("score")) {
            format!("{query} 2026")
        } else {
            query.clone()
        };
        let search_items = perform_web_search(&search_query).await;
        if is_turn_cancelled() {
            return Ok(ChatReply {
                text: String::new(),
                memory_updated: false,
            });
        }
        if !search_items.is_empty() {
            let _ = app.emit("chat-status", json!({ "status": "searched", "detail": format!("Searched {} websites", search_items.len()), "siteCount": search_items.len() }));
            grounding_block = Some(format_grounding_system_context(&query, &search_items));
        }
    }

    if is_turn_cancelled() {
        return Ok(ChatReply {
            text: String::new(),
            memory_updated: false,
        });
    }

    let _ = app.emit("chat-status", json!({ "status": "thinking", "detail": "Thinking…" }));

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
    content.push(json!({ "type": "text", "text": user_text }));

    chat.push(json!({ "role": "user", "content": content }));

    let snapshot = chat.snapshot();
    let window_start = if snapshot.len() > 12 { snapshot.len() - 12 } else { 0 };
    let window_messages: Vec<Value> = snapshot[window_start..].to_vec();

    let mut sys_prompt = system_prompt();
    if let Some(g) = grounding_block {
        sys_prompt.push_str(&g);
    }

    let body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": sys_prompt,
        "stream": true,
        "messages": window_messages,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| e.to_string())?;

    let mut response = client
        .post(ENDPOINT)
        .header("x-api-key", &key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            chat.pop();
            if e.is_timeout() {
                "Request timed out (90s): Claude took too long to respond. The server or connection may be stalled.".to_string()
            } else if e.is_connect() {
                format!("Connection error: Could not reach Anthropic server ({e})")
            } else {
                format!("Network error: {e}")
            }
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        chat.pop();
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

    let mut reply_text = String::new();
    let mut buffer = String::new();

    while let Ok(Some(chunk)) = response.chunk().await {
        if is_turn_cancelled() {
            break;
        }
        let chunk_str = String::from_utf8_lossy(&chunk);
        buffer.push_str(&chunk_str);
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();
            if let Some(json_str) = line.strip_prefix("data: ") {
                let json_trimmed = json_str.trim();
                if let Ok(v) = serde_json::from_str::<Value>(json_trimmed) {
                    if v.get("type").and_then(Value::as_str) == Some("content_block_delta") {
                        if let Some(delta) = v.get("delta").and_then(|d| d.get("text")).and_then(Value::as_str) {
                            if !delta.is_empty() {
                                reply_text.push_str(delta);
                                let _ = app.emit("chat-token", json!({ "token": delta, "done": false }));
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = app.emit("chat-token", json!({ "token": "", "done": true }));

    if is_turn_cancelled() {
        if reply_text.trim().is_empty() {
            chat.pop();
            return Ok(ChatReply {
                text: String::new(),
                memory_updated: false,
            });
        }
    } else if reply_text.trim().is_empty() {
        chat.pop();
        return Err("Claude returned an empty response.".into());
    }

    let (cleaned_reply, memory_updated) = crate::memory::extract_and_save_memory(&query, &reply_text);
    if memory_updated {
        let _ = app.emit("memory-updated", json!({ "updated": true }));
    }
    chat.push(json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": cleaned_reply.clone() }]
    }));
    let _ = crate::memory::append_chat_turn(&query, &cleaned_reply);
    Ok(ChatReply {
        text: cleaned_reply,
        memory_updated,
    })
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
