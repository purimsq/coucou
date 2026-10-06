// Dropped files are copied into %LOCALAPPDATA%\Coucou\inbox so the original is
// never touched and the copy survives the drag source going away.
// The inbox is swept of anything older than a week, as on macOS.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::settings;

const KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFile {
    pub name: String,
    pub path: String,
    pub size: u64,
}

pub fn inbox_dir() -> PathBuf {
    settings::local_dir().join("inbox")
}

pub fn ingest(source: &str) -> Result<DroppedFile, String> {
    let src = Path::new(source);
    let meta = std::fs::metadata(src).map_err(|e| format!("cannot read {source}: {e}"))?;
    if meta.is_dir() {
        return Err("Folders can't be dropped yet.".into());
    }

    let dir = inbox_dir();
    crate::platform::ensure_private_dir(&settings::local_dir()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());

    let mut dest = dir.join(&name);
    if dest.exists() {
        let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = src.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
        for i in 2..1000 {
            let candidate = dir.join(format!("{stem} ({i}){ext}"));
            if !candidate.exists() {
                dest = candidate;
                break;
            }
        }
    }

    std::fs::copy(src, &dest).map_err(|e| format!("cannot copy: {e}"))?;
    // CopyFileEx carries the source's timestamps across, so a file last edited
    // three years ago would arrive already older than the sweep window and be
    // deleted on the spot. The inbox ages from when *we* copied it.
    if let Ok(file) = std::fs::File::options().write(true).open(&dest) {
        let _ = file.set_modified(SystemTime::now());
    }
    sweep(&dir);

    Ok(DroppedFile {
        name,
        path: dest.to_string_lossy().to_string(),
        size: meta.len(),
    })
}

/// Drops anything copied here more than a week ago. `ingest` stamps every copy
/// with the time it landed, so this really is the age of the copy and not the
/// age of whatever the user happened to drag in.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(copied) = meta.modified() else { continue };
        if now.duration_since(copied).map(|age| age > KEEP_FOR).unwrap_or(false) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ── Multi-format File Inspection & Extraction ────────────────────────────────

const MAX_INLINE_TEXT: u64 = 200_000;

#[derive(Debug, Clone)]
pub enum FileContentInfo {
    Text(String),
    Docx(String),
    Pdf { base64: String, text_preview: String },
    Image { media_type: String, base64: String },
}

pub fn inspect_file(path: &str) -> FileContentInfo {
    let p = Path::new(path);
    let ext = p.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "docx" => {
            match extract_docx_text(path) {
                Ok(text) if !text.is_empty() => FileContentInfo::Docx(text),
                _ => FileContentInfo::Text(format!("[Word Document: {path}]")),
            }
        }
        "pdf" => {
            let bytes = std::fs::read(path).unwrap_or_default();
            let b64 = crate::claude::base64_for(&bytes);
            let preview = extract_pdf_readable_text(&bytes);
            FileContentInfo::Pdf { base64: b64, text_preview: preview }
        }
        "jpg" | "jpeg" => {
            let bytes = std::fs::read(path).unwrap_or_default();
            FileContentInfo::Image {
                media_type: "image/jpeg".to_string(),
                base64: crate::claude::base64_for(&bytes),
            }
        }
        "png" => {
            let bytes = std::fs::read(path).unwrap_or_default();
            FileContentInfo::Image {
                media_type: "image/png".to_string(),
                base64: crate::claude::base64_for(&bytes),
            }
        }
        "webp" => {
            let bytes = std::fs::read(path).unwrap_or_default();
            FileContentInfo::Image {
                media_type: "image/webp".to_string(),
                base64: crate::claude::base64_for(&bytes),
            }
        }
        "gif" => {
            let bytes = std::fs::read(path).unwrap_or_default();
            FileContentInfo::Image {
                media_type: "image/gif".to_string(),
                base64: crate::claude::base64_for(&bytes),
            }
        }
        "bmp" => {
            let bytes = std::fs::read(path).unwrap_or_default();
            FileContentInfo::Image {
                media_type: "image/bmp".to_string(),
                base64: crate::claude::base64_for(&bytes),
            }
        }
        "svg" => {
            if let Ok(content) = std::fs::read_to_string(path) {
                FileContentInfo::Text(format!("SVG Vector Image:\n{content}"))
            } else {
                FileContentInfo::Text(format!("[SVG Image: {path}]"))
            }
        }
        _ => {
            // Default text/code reader (up to 200,000 bytes)
            let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if len > MAX_INLINE_TEXT {
                return FileContentInfo::Text(format!("[File too large to inline: {len} bytes]"));
            }
            if let Ok(content) = std::fs::read_to_string(path) {
                FileContentInfo::Text(content)
            } else if let Ok(bytes) = std::fs::read(path) {
                FileContentInfo::Text(String::from_utf8_lossy(&bytes).to_string())
            } else {
                FileContentInfo::Text(String::new())
            }
        }
    }
}

pub fn extract_docx_text(path: &str) -> Result<String, String> {
    let mut cmd = std::process::Command::new("tar");
    cmd.args(["-xOf", path, "word/document.xml"]);
    crate::platform::no_console(&mut cmd);
    let output = cmd.output().map_err(|e| format!("Could not read docx: {e}"))?;
    if !output.status.success() {
        return Err("Could not extract word/document.xml from docx".into());
    }
    let xml = String::from_utf8_lossy(&output.stdout);
    Ok(parse_docx_xml(&xml))
}

pub fn parse_docx_xml(xml: &str) -> String {
    let mut out = String::new();
    let mut in_t = false;
    let mut text_start = 0;

    let bytes = xml.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if in_t && i > text_start {
                out.push_str(&html_decode(&xml[text_start..i]));
            }
            in_t = false;
            if let Some(close_offset) = xml[i..].find('>') {
                let tag = &xml[i + 1..i + close_offset];
                if tag.starts_with("w:t") && !tag.ends_with('/') {
                    in_t = true;
                    text_start = i + close_offset + 1;
                } else if tag.starts_with("w:p") || tag == "/w:p" || tag == "/w:tr" {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                } else if tag == "/w:tc" {
                    if !out.ends_with('\t') && !out.ends_with('\n') {
                        out.push('\t');
                    }
                } else if tag.starts_with("w:br") {
                    out.push('\n');
                } else if tag.starts_with("w:tab") {
                    out.push('\t');
                }
                i += close_offset + 1;
                continue;
            }
        }
        i += 1;
    }
    if in_t && xml.len() > text_start {
        out.push_str(&html_decode(&xml[text_start..]));
    }
    out.trim().to_string()
}

fn html_decode(s: &str) -> String {
    let mut decoded = s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");

    if decoded.contains("&#") {
        let mut result = String::with_capacity(decoded.len());
        let mut rest = decoded.as_str();
        while let Some(start) = rest.find("&#") {
            result.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            if let Some(end) = after.find(';') {
                let ent = &after[..end];
                let ch = if (ent.starts_with('x') || ent.starts_with('X')) && ent.len() > 1 {
                    u32::from_str_radix(&ent[1..], 16).ok().and_then(char::from_u32)
                } else {
                    ent.parse::<u32>().ok().and_then(char::from_u32)
                };
                if let Some(c) = ch {
                    result.push(c);
                } else {
                    result.push_str(&rest[start..start + 2 + end + 1]);
                }
                rest = &after[end + 1..];
            } else {
                result.push_str("&#");
                rest = after;
            }
        }
        result.push_str(rest);
        decoded = result;
    }
    decoded
}

pub fn extract_pdf_readable_text(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut paren_depth = 0;
    let mut current_str = Vec::new();
    let mut escaped = false;

    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if escaped {
            match b {
                b'n' => current_str.push(b'\n'),
                b'r' => current_str.push(b'\r'),
                b't' => current_str.push(b'\t'),
                b'b' => current_str.push(0x08),
                b'f' => current_str.push(0x0C),
                b'(' => current_str.push(b'('),
                b')' => current_str.push(b')'),
                b'\\' => current_str.push(b'\\'),
                b'0'..=b'7' => {
                    let mut oct = (b - b'0') as u8;
                    if i + 1 < len && (b'0'..=b'7').contains(&bytes[i + 1]) {
                        i += 1;
                        oct = oct * 8 + (bytes[i] - b'0');
                        if i + 1 < len && (b'0'..=b'7').contains(&bytes[i + 1]) {
                            i += 1;
                            oct = oct * 8 + (bytes[i] - b'0');
                        }
                    }
                    current_str.push(oct);
                }
                other => current_str.push(other),
            }
            escaped = false;
            i += 1;
            continue;
        }

        if b == b'\\' && paren_depth > 0 {
            escaped = true;
            i += 1;
            continue;
        }

        if b == b'(' {
            if paren_depth > 0 {
                current_str.push(b'(');
            }
            paren_depth += 1;
        } else if b == b')' && paren_depth > 0 {
            paren_depth -= 1;
            if paren_depth == 0 {
                if current_str.len() >= 2 {
                    if let Ok(s) = std::str::from_utf8(&current_str) {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() && trimmed.chars().any(|c| c.is_alphabetic()) {
                            out.push_str(trimmed);
                            out.push(' ');
                        }
                    } else {
                        let ascii: String = current_str
                            .iter()
                            .filter(|&&c| (0x20..=0x7E).contains(&c) || c == b'\n' || c == b'\t')
                            .map(|&c| c as char)
                            .collect();
                        let trimmed = ascii.trim();
                        if !trimmed.is_empty() && trimmed.chars().any(|c| c.is_alphabetic()) {
                            out.push_str(trimmed);
                            out.push(' ');
                        }
                    }
                }
                current_str.clear();
            } else {
                current_str.push(b')');
            }
        } else if paren_depth > 0 {
            current_str.push(b);
        }
        i += 1;
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_copies_and_never_overwrites() {
        let tmp = std::env::temp_dir().join(format!("coucou-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let source = tmp.join("note.txt");
        std::fs::write(&source, b"hello").unwrap();

        let first = ingest(source.to_str().unwrap()).unwrap();
        assert_eq!(first.name, "note.txt");
        assert_eq!(std::fs::read(&first.path).unwrap(), b"hello");

        // A second drop of the same name must not clobber the first copy.
        std::fs::write(&source, b"second").unwrap();
        let second = ingest(source.to_str().unwrap()).unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(std::fs::read(&first.path).unwrap(), b"hello");
        assert_eq!(std::fs::read(&second.path).unwrap(), b"second");

        // Folders are refused rather than silently ignored.
        assert!(ingest(tmp.to_str().unwrap()).is_err());

        // An ancient source must not arrive already older than the sweep window.
        let old_source = tmp.join("ancient.txt");
        std::fs::write(&old_source, b"old").unwrap();
        let long_ago = SystemTime::now() - KEEP_FOR - Duration::from_secs(60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&old_source)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let aged = ingest(old_source.to_str().unwrap()).unwrap();
        assert!(
            Path::new(&aged.path).exists(),
            "a file copied just now was swept as if it were a week old"
        );
        let _ = std::fs::remove_file(&aged.path);

        let _ = std::fs::remove_file(&first.path);
        let _ = std::fs::remove_file(&second.path);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_parse_docx_xml_formatting_and_entities() {
        let xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body>
                <w:p><w:r><w:t>Hello Mochi!</w:t></w:r></w:p>
                <w:p>
                    <w:r><w:t>French café with &amp; &lt;brackets&gt; and &#8220;quotes&#8221;</w:t></w:r>
                    <w:r><w:br/><w:t>Line 2 after break</w:t></w:r>
                </w:p>
                <w:tbl>
                    <w:tr>
                        <w:tc><w:p><w:r><w:t>Column A</w:t></w:r></w:p></w:tc>
                        <w:tc><w:p><w:r><w:t>Column B</w:t></w:r></w:p></w:tc>
                    </w:tr>
                </w:tbl>
            </w:body>
        </w:document>"#;
        let text = parse_docx_xml(xml);
        assert!(text.contains("Hello Mochi!"));
        assert!(text.contains("French café with & <brackets> and “quotes”"));
        assert!(text.contains("Line 2 after break"));
        assert!(text.contains("Column A"));
        assert!(text.contains("Column B"));
    }

    #[test]
    fn test_extract_pdf_readable_text() {
        let fake_pdf = b"%PDF-1.4\n1 0 obj\n<< /Length 50 >>\nstream\nBT\n/F1 12 Tf\n(Hello from PDF test with \\(nested\\) parens) Tj\nET\nendstream\nendobj\nxref\ntrailer\n<< /Root 1 0 R >>\n%%EOF";
        let text = extract_pdf_readable_text(fake_pdf);
        assert!(text.contains("Hello from PDF test with (nested) parens"));
    }
}
