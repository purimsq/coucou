// Persistent chat history and long-term memory / user profile for Mochi.
//
// Chat history is saved to %APPDATA%\Coucou\chat_history.json.
// When the user clicks the "Clear" button in chat, this file and in-memory
// history are wiped clean.
//
// The User Profile & Memory is saved to %APPDATA%\Coucou\user_profile.json.
// This file is NEVER wiped when clearing chat history, allowing Mochi to
// remember the user's name, preferences, favorite teams, and learned facts
// permanently.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

use crate::platform::ensure_private_dir;
use crate::settings::config_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageEntry {
    pub id: u64,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFact {
    pub id: String,
    pub category: String, // "identity" | "preference" | "interest" | "note"
    pub text: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub name: String,
    pub notes: String,
    pub facts: Vec<MemoryFact>,
    pub updated_at: String,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            notes: String::new(),
            facts: Vec::new(),
            updated_at: current_timestamp(),
        }
    }
}

fn current_timestamp() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(dur) => dur.as_secs().to_string(),
        Err(_) => "0".to_string(),
    }
}

// ── Persistent Chat History ──────────────────────────────────────────────────

fn chat_history_path() -> PathBuf {
    config_dir().join("chat_history.json")
}

pub fn load_chat() -> Vec<ChatMessageEntry> {
    let path = chat_history_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn save_chat(messages: &[ChatMessageEntry]) -> Result<(), String> {
    let dir = config_dir();
    ensure_private_dir(&dir).map_err(|e| format!("Failed to create config dir: {e}"))?;
    let json = serde_json::to_vec_pretty(messages)
        .map_err(|e| format!("Failed to serialize chat history: {e}"))?;
    std::fs::write(chat_history_path(), json)
        .map_err(|e| format!("Failed to write chat history: {e}"))
}

pub fn clear_chat() -> Result<(), String> {
    let path = chat_history_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to remove chat history file: {e}"))?;
    }
    Ok(())
}

pub fn append_chat_turn(user_content: &str, assistant_content: &str) -> Result<(), String> {
    let mut history = load_chat();
    let next_id = history.last().map(|m| m.id + 1).unwrap_or(1);
    history.push(ChatMessageEntry {
        id: next_id,
        role: "user".to_string(),
        content: user_content.to_string(),
    });
    history.push(ChatMessageEntry {
        id: next_id + 1,
        role: "assistant".to_string(),
        content: assistant_content.to_string(),
    });
    save_chat(&history)
}

// ── Persistent User Profile & Long-Term Memory ───────────────────────────────

fn user_profile_path() -> PathBuf {
    config_dir().join("user_profile.json")
}

pub fn load_profile() -> UserProfile {
    let path = user_profile_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => UserProfile::default(),
    }
}

pub fn save_profile(profile: &UserProfile) -> Result<(), String> {
    let dir = config_dir();
    ensure_private_dir(&dir).map_err(|e| format!("Failed to create config dir: {e}"))?;
    let json = serde_json::to_vec_pretty(profile)
        .map_err(|e| format!("Failed to serialize user profile: {e}"))?;
    std::fs::write(user_profile_path(), json)
        .map_err(|e| format!("Failed to write user profile: {e}"))
}

pub fn clear_profile() -> Result<(), String> {
    let path = user_profile_path();
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Failed to remove user profile file: {e}"))?;
    }
    Ok(())
}

pub fn add_or_update_fact(category: &str, text: &str) -> Result<UserProfile, String> {
    let mut profile = load_profile();
    let text = text.trim();
    if text.is_empty() {
        return Ok(profile);
    }

    let cat = category.trim().to_lowercase();
    let normalized_cat = match cat.as_str() {
        "identity" | "preference" | "interest" | "note" => cat,
        _ => "preference".to_string(),
    };

    let now = current_timestamp();
    let existing_idx = profile.facts.iter().position(|f| {
        f.text.trim().eq_ignore_ascii_case(text)
    });

    if let Some(idx) = existing_idx {
        profile.facts[idx].category = normalized_cat;
        profile.facts[idx].updated_at = now.clone();
    } else {
        let fact_id = format!("fact_{}_{}", profile.facts.len() + 1, now);
        profile.facts.push(MemoryFact {
            id: fact_id,
            category: normalized_cat,
            text: text.to_string(),
            updated_at: now.clone(),
        });
    }

    profile.updated_at = now;
    save_profile(&profile)?;
    Ok(profile)
}

pub fn delete_fact(fact_id: &str) -> Result<UserProfile, String> {
    let mut profile = load_profile();
    profile.facts.retain(|f| f.id != fact_id);
    profile.updated_at = current_timestamp();
    save_profile(&profile)?;
    Ok(profile)
}

pub fn format_memory_for_prompt() -> String {
    let profile = load_profile();
    let mut lines = Vec::new();

    if !profile.name.trim().is_empty() {
        lines.push(format!("• User's Name: {}", profile.name.trim()));
    }
    if !profile.notes.trim().is_empty() {
        lines.push(format!("• Special Notes: {}", profile.notes.trim()));
    }

    for fact in &profile.facts {
        lines.push(format!("• [{}] {}", fact.category, fact.text.trim()));
    }

    if lines.is_empty() {
        return String::new();
    }

    format!(
        "[Mochi's Long-Term Memory & User Profile]\n\
         The following information is permanently stored in your user profile. \
         It is preserved even when the user clicks 'Clear Chat':\n\
         {}\n\n\
         INSTRUCTION FOR MEMORY EXTRACTION:\n\
         When the user shares personal details, preferences, favorite things, or tells you to remember something, \
         append a memory tag at the very end of your response:\n\
         <remember category=\"preference\">detail to remember</remember>\n\
         Valid categories: identity, preference, interest, note.\n\
         Only output a memory tag when there is genuinely something new or updated to remember. \
         The system silently strips the tag and saves it to your permanent user profile.",
        lines.join("\n")
    )
}

/// Parses and extracts any `<remember category="...">...</remember>` tags
/// from the assistant response. Automatically stores them in `user_profile.json`
/// and returns the cleaned text without the tags.
pub fn extract_and_save_memory(raw_text: &str) -> String {
    let mut cleaned = String::new();
    let mut remaining = raw_text;

    while let Some(start_idx) = remaining.find("<remember") {
        cleaned.push_str(&remaining[..start_idx]);
        let after_start = &remaining[start_idx..];
        if let Some(tag_close) = after_start.find('>') {
            let tag_header = &after_start[..tag_close];
            let after_tag = &after_start[tag_close + 1..];
            if let Some(end_idx) = after_tag.find("</remember>") {
                let fact_content = after_tag[..end_idx].trim();
                let category = if let Some(cat_start) = tag_header.find("category=\"") {
                    let cat_val = &tag_header[cat_start + 10..];
                    if let Some(cat_end) = cat_val.find('"') {
                        &cat_val[..cat_end]
                    } else {
                        "preference"
                    }
                } else if let Some(cat_start) = tag_header.find("category='") {
                    let cat_val = &tag_header[cat_start + 10..];
                    if let Some(cat_end) = cat_val.find('\'') {
                        &cat_val[..cat_end]
                    } else {
                        "preference"
                    }
                } else {
                    "preference"
                };

                if !fact_content.is_empty() {
                    let _ = add_or_update_fact(category, fact_content);
                }

                remaining = &after_tag[end_idx + 11..];
                continue;
            }
        }
        // If malformed, advance past "<remember" to avoid infinite loop
        cleaned.push_str("<remember");
        remaining = after_start;
    }
    cleaned.push_str(remaining);
    cleaned.trim().to_string()
}
