//! Picture previews: find the picture behind `[Image #N]` or an image file path.
//!
//! Where pictures come from:
//! - `[Image #N]`: Claude Code stores every pasted picture inside the chat log
//!   (`~/.claude/projects/<project>/<chat id>.jsonl`), in the same message as its
//!   `[Image #N]` text, as base64. Numbers count up across the whole chat.
//! - A picture pasted but not sent yet is not in the log. Grace keeps its own copy
//!   from the clipboard at the moment you paste it (see `Pending`).
//! - A file path like `C:\shots\a.png`: read from disk.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

pub const IMAGE_EXTS: [&str; 9] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif", "tiff"];

pub fn is_image_path(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    IMAGE_EXTS.iter().any(|e| lower.ends_with(&format!(".{e}")))
}

fn tag_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\[Image #(\d+)\]").unwrap())
}

/// `[Image #N]` under column `col` of a screen line: (N, first col, last col).
pub fn tag_at(line: &str, col: usize) -> Option<(u32, usize, usize)> {
    // Work in characters (= terminal columns for this ASCII text).
    let chars: Vec<char> = line.chars().collect();
    let s: String = chars.iter().collect();
    for m in tag_re().captures_iter(&s) {
        let all = m.get(0)?;
        let start = s[..all.start()].chars().count();
        let end = start + all.as_str().chars().count() - 1;
        if col >= start && col <= end {
            return Some((m[1].parse().ok()?, start, end));
        }
    }
    None
}

pub fn transcript_path(session: &str) -> Option<PathBuf> {
    if session.is_empty() {
        return None;
    }
    let dirs = std::fs::read_dir(crate::procmon::claude_dir().join("projects")).ok()?;
    let file = format!("{session}.jsonl");
    dirs.flatten().map(|d| d.path().join(&file)).find(|p| p.exists())
}

/// Where each `[Image #N]` of one chat is stored in its log: N -> (line start, which picture in that message).
#[derive(Default)]
pub struct ChatImages {
    path: PathBuf,
    scanned: u64,
    pub map: HashMap<u32, (u64, usize)>,
    pub max_n: u32,
}

impl ChatImages {
    pub fn new(path: PathBuf) -> ChatImages {
        ChatImages { path, ..Default::default() }
    }

    /// Read the part of the log written since last time (the log only grows).
    pub fn update(&mut self) {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.scanned {
            // The log was rewritten: start again.
            self.scanned = 0;
            self.map.clear();
            self.max_n = 0;
        }
        if len == self.scanned || f.seek(SeekFrom::Start(self.scanned)).is_err() {
            return;
        }
        let mut buf = Vec::with_capacity((len - self.scanned) as usize);
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        // Only whole lines; a line still being written is read next time.
        let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else { return };
        let mut pos = 0usize;
        while pos <= last_nl {
            let end = buf[pos..=last_nl].iter().position(|b| *b == b'\n').map(|i| pos + i).unwrap_or(last_nl);
            let line = &buf[pos..end];
            let offset = self.scanned + pos as u64;
            if contains(line, b"[Image #") && contains(line, b"\"image\"") {
                self.index_line(line, offset);
            }
            pos = end + 1;
        }
        self.scanned += (last_nl + 1) as u64;
    }

    fn index_line(&mut self, line: &[u8], offset: u64) {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else { return };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            return;
        }
        let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) else { return };
        // The tags in the text, in order, belong to the picture blocks, in order.
        let mut tags = Vec::new();
        let mut pictures = 0usize;
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    let t = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    for m in tag_re().captures_iter(t) {
                        if let Ok(n) = m[1].parse::<u32>() {
                            tags.push(n);
                        }
                    }
                }
                Some("image") => pictures += 1,
                _ => {}
            }
        }
        for (k, n) in tags.into_iter().enumerate().take(pictures) {
            self.map.insert(n, (offset, k));
            self.max_n = self.max_n.max(n);
        }
    }

    /// The picture's file bytes (PNG, JPEG ...).
    pub fn bytes(&self, n: u32) -> Option<Vec<u8>> {
        let (offset, k) = *self.map.get(&n)?;
        let mut f = std::fs::File::open(&self.path).ok()?;
        f.seek(SeekFrom::Start(offset)).ok()?;
        let mut line = Vec::new();
        let mut chunk = [0u8; 1 << 16];
        loop {
            let got = f.read(&mut chunk).ok()?;
            if got == 0 {
                break;
            }
            if let Some(i) = chunk[..got].iter().position(|b| *b == b'\n') {
                line.extend_from_slice(&chunk[..i]);
                break;
            }
            line.extend_from_slice(&chunk[..got]);
        }
        let v: serde_json::Value = serde_json::from_slice(&line).ok()?;
        let blocks = v.pointer("/message/content")?.as_array()?;
        let img = blocks.iter().filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("image")).nth(k)?;
        let data = img.pointer("/source/data")?.as_str()?;
        base64_decode(data)
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The picture on the clipboard as file bytes (PNG if offered, else a BMP made from the bitmap).
pub fn clipboard_image(hwnd: windows::Win32::Foundation::HWND) -> Option<Vec<u8>> {
    use windows::core::w;
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    unsafe fn global_bytes(h: windows::Win32::Foundation::HANDLE) -> Option<Vec<u8>> {
        let g = HGLOBAL(h.0);
        let n = GlobalSize(g);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() || n == 0 {
            return None;
        }
        let v = std::slice::from_raw_parts(p, n).to_vec();
        let _ = GlobalUnlock(g);
        Some(v)
    }
    unsafe {
        let png = RegisterClipboardFormatW(w!("PNG"));
        OpenClipboard(Some(hwnd)).ok()?;
        let out = (|| {
            if IsClipboardFormatAvailable(png).is_ok() {
                if let Some(b) = GetClipboardData(png).ok().and_then(|h| global_bytes(h)) {
                    return Some(b);
                }
            }
            let dib = global_bytes(GetClipboardData(crate::win::CF_DIB).ok()?)?;
            if dib.len() < 40 {
                return None;
            }
            // A BMP file is the clipboard bitmap with a 14-byte header in front.
            let rd = |o: usize| u32::from_le_bytes([dib[o], dib[o + 1], dib[o + 2], dib[o + 3]]);
            let header = rd(0) as usize;
            let bpp = u16::from_le_bytes([dib[14], dib[15]]) as u32;
            let compression = rd(16);
            let used = rd(32);
            let colors = if used > 0 { used } else if bpp <= 8 { 1 << bpp } else { 0 };
            let masks = if compression == 3 && header == 40 { 12 } else { 0 };
            let offset = 14 + header + masks + colors as usize * 4;
            let mut f = Vec::with_capacity(14 + dib.len());
            f.extend_from_slice(b"BM");
            f.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
            f.extend_from_slice(&[0, 0, 0, 0]);
            f.extend_from_slice(&(offset as u32).to_le_bytes());
            f.extend_from_slice(&dib);
            Some(f)
        })();
        let _ = CloseClipboard();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_tag_under_the_mouse() {
        let line = "  ❯ fix this [Image #12] and [Image #3] now";
        let at = line.chars().position(|c| c == '#').unwrap();
        assert_eq!(tag_at(line, at).map(|t| t.0), Some(12));
        let second = line.chars().collect::<String>().rfind("#3").unwrap();
        let col = line[..second].chars().count();
        assert_eq!(tag_at(line, col).map(|t| t.0), Some(3));
        assert_eq!(tag_at(line, 2), None);
    }

    #[test]
    fn base64_round_trip() {
        assert_eq!(base64_decode("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64_decode("iVBORw0KGgo=").unwrap()[..4], [0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn image_paths() {
        assert!(is_image_path(r"C:\shots\Screen 1.PNG"));
        assert!(is_image_path("out/chart.webp"));
        assert!(!is_image_path("src/app.rs"));
    }

    #[test]
    fn indexes_a_chat_log() {
        let dir = std::env::temp_dir().join(format!("pt-img-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("chat.jsonl");
        let msg = |n: u32| {
            format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"look [Image #{n}] here"}},{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}}}]}}}}"#
            )
        };
        std::fs::write(&p, format!("{{\"type\":\"summary\"}}\n{}\n", msg(6))).unwrap();
        let mut c = ChatImages::new(p.clone());
        c.update();
        assert_eq!(c.max_n, 6);
        assert_eq!(c.bytes(6).unwrap(), b"hello");
        // The log grows: only the new part is read.
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        use std::io::Write;
        writeln!(f, "{}", msg(7)).unwrap();
        c.update();
        assert_eq!(c.max_n, 7);
        assert!(c.bytes(7).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
