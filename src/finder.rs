//! Find any old Claude chat: Ctrl+Shift+P (or the ⌄ menu next to +).
//!
//! A search box over every Claude chat on this PC, in every project, not only the open tabs.
//! Type a few words ("whatsapp webhook"); pick a chat; it opens as a sleeping tab (0 MB) in its
//! own folder. Press Enter there to continue it. A chat that is already a tab is just shown.
//!
//! Where the text comes from: Claude keeps every chat in `~/.claude/projects/<project>/<id>.jsonl`.
//! A background thread reads them once (about 900 MB on this PC, a few seconds), keeps a small
//! search text per chat (your messages and Claude's answers, the newest 32 KB of each), and saves
//! it in `%APPDATA%\Grace\chat_index.json`. After that it only reads what the logs gained.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::app::{Alert, App, Tab};
use crate::layout::{Node, Rect};
use crate::pane::Dormant;
use crate::theme::*;

/// Search text kept per chat, for your messages and for Claude's answers (the newest part).
const KEEP: usize = 32 * 1024;
/// Results listed at most.
const MAX_RESULTS: usize = 60;
/// Rows shown at once in the box.
const ROWS: usize = 8;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ChatEntry {
    pub session: String,
    pub path: PathBuf,
    /// Claude's title for the chat (or your own rename).
    pub title: String,
    renamed: bool,
    pub cwd: String,
    pub first_prompt: String,
    /// Your messages, newest last.
    prompts: String,
    /// Claude's answers, newest last.
    answers: String,
    /// When the log last changed (seconds since 1970).
    pub modified: u64,
    /// Bytes of the log read so far (it only grows; the rest is read next time).
    read: u64,
    #[serde(skip)]
    lower: String,
}

impl ChatEntry {
    pub fn name(&self) -> &str {
        if !self.title.is_empty() {
            &self.title
        } else if !self.first_prompt.is_empty() {
            &self.first_prompt
        } else {
            "Untitled chat"
        }
    }

    /// Lower-case copy of everything searched: title, then your messages, then Claude's.
    /// ASCII-only lowering keeps every byte where it was, so a match can be cut out of the
    /// original text for the snippet.
    fn update_lower(&mut self) {
        self.lower = format!("{}\n{}\n{}\n{}", self.title, self.first_prompt, self.prompts, self.answers).to_ascii_lowercase();
    }
}

#[derive(Default)]
pub struct Index {
    pub chats: Vec<ChatEntry>,
    /// Reading the logs right now: (done, total).
    pub busy: Option<(usize, usize)>,
    /// Grows on every change, so the open search box knows to search again.
    pub version: u64,
    loaded: bool,
}

pub type Shared = Arc<Mutex<Index>>;

fn cache_path() -> PathBuf {
    crate::config::data_dir().join("chat_index.json")
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Keep only the newest `KEEP` bytes of `s` (cut at a character edge).
fn keep_tail(s: &mut String) {
    if s.len() > KEEP {
        let mut cut = s.len() - KEEP;
        while !s.is_char_boundary(cut) {
            cut += 1;
        }
        s.drain(..cut);
    }
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The text blocks of a message ("content" is a string or a list of blocks).
fn texts(content: &serde_json::Value) -> Vec<&str> {
    match content {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(a) => a
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Read one line of a chat log into `e`.
pub fn add_line(e: &mut ChatEntry, line: &[u8]) {
    let title = has(line, b"-title\"");
    let user = has(line, b"\"type\":\"user\"");
    let assistant = has(line, b"\"type\":\"assistant\"");
    if !title && !user && !assistant {
        return;
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else { return };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("custom-title") => {
            if let Some(t) = v.get("customTitle").and_then(|t| t.as_str()) {
                e.title = t.to_string();
                e.renamed = true;
            }
        }
        Some("ai-title") if !e.renamed => {
            if let Some(t) = v.get("aiTitle").and_then(|t| t.as_str()) {
                e.title = t.to_string();
            }
        }
        Some("user") => {
            if e.cwd.is_empty() {
                if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
                    e.cwd = c.to_string();
                }
            }
            if v.get("isMeta").and_then(|m| m.as_bool()) == Some(true) || v.get("isCompactSummary").is_some() {
                return;
            }
            let Some(content) = v.pointer("/message/content") else { return };
            for t in texts(content) {
                let t = t.trim();
                // Claude's own notes in your turn ("<command-name>", "<system-reminder>"...).
                if t.is_empty() || t.starts_with('<') || t.starts_with("[Request interrupted") {
                    continue;
                }
                if e.first_prompt.is_empty() {
                    e.first_prompt = t.lines().next().unwrap_or("").chars().take(120).collect();
                }
                e.prompts.push_str(t);
                e.prompts.push('\n');
                keep_tail(&mut e.prompts);
            }
        }
        Some("assistant") => {
            let Some(content) = v.pointer("/message/content") else { return };
            for t in texts(content) {
                e.answers.push_str(t.trim());
                e.answers.push('\n');
                keep_tail(&mut e.answers);
            }
        }
        _ => {}
    }
}

/// Read what `e`'s log gained since last time. Returns false when nothing changed.
fn read_log(e: &mut ChatEntry, len: u64) -> bool {
    if len == e.read {
        return false;
    }
    if len < e.read {
        // Rewritten: start again.
        *e = ChatEntry { session: e.session.clone(), path: e.path.clone(), ..Default::default() };
    }
    let Ok(mut f) = std::fs::File::open(&e.path) else { return false };
    if f.seek(SeekFrom::Start(e.read)).is_err() {
        return false;
    }
    let mut r = BufReader::with_capacity(1 << 16, f);
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                // A line still being written is read next time.
                if line.last() != Some(&b'\n') {
                    break;
                }
                add_line(e, &line);
                e.read += n as u64;
            }
        }
    }
    true
}

/// Read the logs that changed (in the background). Calls `done` when something changed.
pub fn refresh(shared: Shared, done: fn()) {
    {
        let mut ix = shared.lock().unwrap();
        if ix.busy.is_some() {
            return;
        }
        ix.busy = Some((0, 0));
    }
    let _ = std::thread::Builder::new().name("chat-finder".into()).spawn(move || {
        // The saved index first (fast), so the box has results at once.
        let loaded = shared.lock().unwrap().loaded;
        if !loaded {
            let saved: Vec<ChatEntry> = std::fs::read(cache_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
            let mut ix = shared.lock().unwrap();
            ix.chats = saved.into_iter().map(|mut c| {
                c.update_lower();
                c
            }).collect();
            ix.loaded = true;
            ix.version += 1;
            drop(ix);
            done();
        }
        // Every chat log on this PC: projects/<project>/<chat id>.jsonl.
        let mut files = Vec::new();
        if let Ok(dirs) = std::fs::read_dir(crate::procmon::claude_dir().join("projects")) {
            for d in dirs.flatten() {
                if let Ok(fs) = std::fs::read_dir(d.path()) {
                    for f in fs.flatten() {
                        let p = f.path();
                        if p.extension().is_some_and(|x| x == "jsonl") {
                            if let Ok(m) = f.metadata() {
                                files.push((p, m.len(), m.modified().map(secs).unwrap_or(0)));
                            }
                        }
                    }
                }
            }
        }
        let total = files.len();
        let mut changed = false;
        for (k, (path, len, modified)) in files.iter().enumerate() {
            let session = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            // Work on a copy, so the box keeps searching the old one meanwhile.
            let mut e = {
                let ix = shared.lock().unwrap();
                ix.chats.iter().find(|c| c.session == session).cloned()
            }
            .unwrap_or(ChatEntry { session: session.clone(), path: path.clone(), ..Default::default() });
            e.path = path.clone();
            e.modified = *modified;
            if read_log(&mut e, *len) {
                e.update_lower();
                changed = true;
                let mut ix = shared.lock().unwrap();
                match ix.chats.iter_mut().find(|c| c.session == session) {
                    Some(c) => *c = e,
                    None => ix.chats.push(e),
                }
                ix.version += 1;
            }
            shared.lock().unwrap().busy = Some((k + 1, total));
            if k % 4 == 3 {
                done();
            }
        }
        let mut ix = shared.lock().unwrap();
        // Chats whose log is gone.
        let before = ix.chats.len();
        ix.chats.retain(|c| files.iter().any(|(p, _, _)| *p == c.path));
        changed |= ix.chats.len() != before;
        ix.busy = None;
        ix.version += 1;
        if changed {
            if let Ok(b) = serde_json::to_vec(&ix.chats) {
                let tmp = cache_path().with_extension("tmp");
                if std::fs::write(&tmp, b).is_ok() {
                    let _ = std::fs::rename(&tmp, cache_path());
                }
            }
        }
        drop(ix);
        done();
    });
}

/// One line in the list.
#[derive(Clone, Debug)]
pub struct Hit {
    pub session: String,
    pub title: String,
    pub cwd: String,
    pub modified: u64,
    /// The words around the first match ("" when the title matched or there is no query).
    pub snippet: String,
}

/// Chats that have every word of `query` (any case), best first: title matches, then your
/// messages, then Claude's answers; newer first. No query = newest chats.
pub fn search(chats: &[ChatEntry], query: &str) -> Vec<Hit> {
    let words: Vec<String> = query.split_whitespace().map(|w| w.to_ascii_lowercase()).collect();
    let mut scored: Vec<(u32, &ChatEntry, String)> = Vec::new();
    for c in chats {
        if words.iter().any(|w| !c.lower.contains(w.as_str())) {
            continue;
        }
        let title_end = c.title.len() + 1 + c.first_prompt.len();
        let prompts_end = title_end + 1 + c.prompts.len();
        let mut score = 0;
        let mut snippet = String::new();
        for w in &words {
            let at = c.lower.find(w.as_str()).unwrap_or(0);
            score += if at < title_end { 100 } else if at < prompts_end { 10 } else { 1 };
            if snippet.is_empty() && at >= title_end {
                snippet = around(&c.lower, &format!("{}\n{}\n{}\n{}", c.title, c.first_prompt, c.prompts, c.answers), at, w.len());
            }
        }
        scored.push((score, c, snippet));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.modified.cmp(&a.1.modified)));
    scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, c, snippet)| Hit { session: c.session.clone(), title: c.name().to_string(), cwd: c.cwd.clone(), modified: c.modified, snippet })
        .collect()
}

/// About 90 characters of `text` around byte `at`, on one line.
fn around(_lower: &str, text: &str, at: usize, len: usize) -> String {
    let mut a = at.saturating_sub(30);
    while a > 0 && !text.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = (at + len + 60).min(text.len());
    while b < text.len() && !text.is_char_boundary(b) {
        b += 1;
    }
    let mid: String = text[a..b].split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}{}{}", if a > 0 { "…" } else { "" }, mid, if b < text.len() { "…" } else { "" })
}

/// "5 min ago", "3 h ago", "2 d ago", then "18 Nov".
pub fn ago(modified: u64, now: u64) -> String {
    let d = now.saturating_sub(modified);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=604_799 => format!("{} d ago", d / 86_400),
        _ => {
            // Day and month from the date (civil-from-days, no time zone: close enough).
            let days = (modified / 86_400) as i64;
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let day = doy - (153 * mp + 2) / 5 + 1;
            let month = if mp < 10 { mp + 3 } else { mp - 9 };
            let names = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            format!("{day} {}", names[(month - 1) as usize])
        }
    }
}

/// The open search box.
#[derive(Default)]
pub struct Finder {
    pub query: String,
    pub sel: usize,
    pub top: usize,
    pub hits: Vec<Hit>,
    /// The index version `hits` came from.
    seen: u64,
    /// Rows drawn in the last frame: (rect, index in `hits`).
    pub rows: Vec<(Rect, usize)>,
    pub card: Rect,
}

impl App {
    pub fn open_finder(&mut self) {
        if self.ui.finder.take().is_some() {
            self.invalidate();
            return;
        }
        self.ui.finder = Some(Finder { seen: u64::MAX, ..Default::default() });
        refresh(self.chat_index.clone(), crate::pane::redraw_soon);
        self.finder_search();
    }

    pub fn finder_search(&mut self) {
        let Some(f) = &mut self.ui.finder else { return };
        let ix = self.chat_index.lock().unwrap();
        f.hits = search(&ix.chats, &f.query);
        f.seen = ix.version;
        f.sel = 0;
        f.top = 0;
        drop(ix);
        self.invalidate();
    }

    /// Keys while the box is open. Returns true when used (letters come as characters).
    pub fn finder_key(&mut self, vk: u16, ctrl: bool) -> bool {
        use crate::input::*;
        let Some(f) = &mut self.ui.finder else { return false };
        let n = f.hits.len();
        match vk {
            VK_ESCAPE => self.ui.finder = None,
            VK_RETURN => {
                let pick = f.hits.get(f.sel).cloned();
                if let Some(h) = pick {
                    self.finder_open(&h);
                }
            }
            VK_DOWN if n > 0 => f.sel = (f.sel + 1).min(n - 1),
            VK_UP => f.sel = f.sel.saturating_sub(1),
            VK_NEXT if n > 0 => f.sel = (f.sel + ROWS).min(n - 1),
            VK_PRIOR => f.sel = f.sel.saturating_sub(ROWS),
            VK_BACK => {
                if ctrl {
                    let t = f.query.trim_end().len();
                    let cut = f.query[..t].rfind(' ').map_or(0, |i| i + 1);
                    f.query.truncate(cut);
                } else {
                    f.query.pop();
                }
                self.finder_search();
                return true;
            }
            _ if ctrl && vk == 0x50 => self.ui.finder = None, // Ctrl+Shift+P again
            // Letters arrive as characters (finder_char). Ctrl+keys do nothing here.
            _ => return ctrl,
        }
        self.invalidate();
        true
    }

    pub fn finder_char(&mut self, s: &str) -> bool {
        let Some(f) = &mut self.ui.finder else { return false };
        if !s.chars().any(|c| c.is_control()) {
            f.query.push_str(s);
            self.finder_search();
        }
        true
    }

    /// Mouse press while the box is open: a row opens that chat; outside closes the box.
    pub fn finder_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let Some(f) = &self.ui.finder else { return false };
        if let Some((_, i)) = f.rows.iter().find(|(r, _)| r.contains(x, y)) {
            if let Some(h) = f.hits.get(*i).cloned() {
                self.finder_open(&h);
            }
        } else if !f.card.contains(x, y) {
            self.ui.finder = None;
        }
        self.invalidate();
        true
    }

    pub fn finder_wheel(&mut self, delta: i16) -> bool {
        let Some(f) = &mut self.ui.finder else { return false };
        let n = f.hits.len();
        if delta < 0 {
            f.sel = (f.sel + 3).min(n.saturating_sub(1));
        } else {
            f.sel = f.sel.saturating_sub(3);
        }
        self.invalidate();
        true
    }

    /// Show that chat: its tab if it has one, else a new sleeping tab in its folder.
    pub fn finder_open(&mut self, h: &Hit) {
        self.ui.finder = None;
        let has = |p: &crate::pane::Pane| {
            p.stats.claude.as_ref().is_some_and(|c| c.session_id == h.session)
                || p.dormant.as_ref().and_then(|d| d.claude_session.as_deref()) == Some(h.session.as_str())
        };
        if let Some(id) = self.panes.values().find(|p| has(p)).map(|p| p.id) {
            if let Some(ti) = self.tab_of(id) {
                self.activate(ti);
                self.focus_pane(id);
            }
            self.show_toast("That chat is already open here.".into());
            return;
        }
        // Running in another window (Warp...)? Then Grace asks before opening it twice.
        let ours: Vec<u32> = self.panes.values().filter_map(|p| p.stats.claude.as_ref().map(|c| c.pid)).collect();
        let elsewhere = crate::procmon::other_chats(&ours).into_iter().find(|c| c.session_id == h.session).map(|c| c.pid);
        let cwd = Some(h.cwd.clone()).filter(|d| !d.is_empty() && std::path::Path::new(d).is_dir());
        let prof = self.claude_profile();
        let id = self.add_pane(&prof, cwd);
        if let Some(p) = self.panes.get_mut(&id) {
            p.dormant = Some(Dormant { claude_session: Some(h.session.clone()), claude_name: Some(h.title.clone()), freed_mb: None, elsewhere });
        }
        let tid = self.id();
        self.tabs.push(Tab { id: tid, root: Node::Leaf(id), focus: id, custom_title: None, alert: Alert::None, zoom: None });
        let ti = self.tabs.len() - 1;
        self.activate(ti);
        self.relayout();
        self.show_toast("Press Enter to continue this chat.".into());
    }

    pub fn paint_finder(&mut self) {
        let Some(mut f) = self.ui.finder.take() else { return };
        // Search again when the index changed (it is read in the background).
        let (version, busy) = {
            let ix = self.chat_index.lock().unwrap();
            (ix.version, ix.busy)
        };
        if version != f.seen {
            let ix = self.chat_index.lock().unwrap();
            let keep = f.hits.get(f.sel).map(|h| h.session.clone());
            f.hits = search(&ix.chats, &f.query);
            f.seen = version;
            f.sel = keep.and_then(|s| f.hits.iter().position(|h| h.session == s)).unwrap_or(0);
        }
        let g = &self.gfx;
        let s = g.scale;
        let (w, h) = self.client();
        // Dim everything behind the box.
        g.fill_a(Rect::new(0.0, 0.0, w, h), BAR, 0.55);
        let cw = (760.0 * s).min(w - 40.0 * s);
        let row_h = (46.0 * s).round();
        let input_h = (44.0 * s).round();
        let foot_h = (28.0 * s).round();
        let shown = f.hits.len().min(ROWS);
        let list_h = if shown == 0 { row_h } else { shown as f32 * row_h };
        let card = Rect::new(((w - cw) / 2.0).round(), (self.bar_h() + 36.0 * s).round(), cw, input_h + list_h + foot_h);
        f.card = card;
        g.round(card, 10.0 * s, CARD, 1.0);
        g.outline(card, 10.0 * s, BORDER, 1.0);
        // The query line.
        let pad = 16.0 * s;
        let q = if f.query.is_empty() { "Search all your Claude chats…".to_string() } else { format!("{}▏", f.query) };
        g.text(&q, &g.heads[1], Rect::new(card.x + pad, card.y, card.w - 2.0 * pad - 150.0 * s, input_h), if f.query.is_empty() { TEXT_DIM } else { TEXT });
        let status = match busy {
            Some((d, t)) if t > 0 => format!("reading chats {d} / {t}"),
            Some(_) => "reading chats…".into(),
            None => format!("{} chats", self.chat_index.lock().unwrap().chats.len()),
        };
        let sw = g.measure(&status, &g.ui_small);
        g.text(&status, &g.ui_small, Rect::new(card.right() - pad - sw, card.y, sw + 2.0, input_h), TEXT_DIM);
        g.fill(Rect::new(card.x, card.y + input_h, card.w, 1.0), BORDER);
        // The list: keep the selected row in view.
        if f.sel < f.top {
            f.top = f.sel;
        }
        if f.sel >= f.top + ROWS {
            f.top = f.sel + 1 - ROWS;
        }
        f.rows.clear();
        let now = secs(SystemTime::now());
        let (mx, my) = self.ui.mouse;
        let y0 = card.y + input_h;
        if f.hits.is_empty() {
            let msg = if busy.is_some() && f.query.is_empty() { "Reading your chats…" } else { "No chat has all these words." };
            g.text(msg, &g.ui, Rect::new(card.x + pad, y0, card.w - 2.0 * pad, row_h), TEXT_DIM);
        }
        for (k, i) in (f.top..f.hits.len()).take(ROWS).enumerate() {
            let hit = &f.hits[i];
            let r = Rect::new(card.x + 6.0 * s, y0 + k as f32 * row_h + 3.0 * s, card.w - 12.0 * s, row_h - 6.0 * s);
            if r.contains(mx, my) {
                f.sel = i;
            }
            if i == f.sel {
                g.round(r, 6.0 * s, SELECTION, 0.55);
            }
            let when = ago(hit.modified, now);
            let folder = crate::folders::short_name(&hit.cwd);
            let right = format!("{folder} · {when}");
            let rw = g.measure(&right, &g.ui_small);
            let half = r.h / 2.0;
            g.text(&hit.title, &g.ui_bold, Rect::new(r.x + 10.0 * s, r.y + 2.0 * s, r.w - rw - 34.0 * s, half), TEXT);
            g.text(&right, &g.ui_small, Rect::new(r.right() - rw - 10.0 * s, r.y + 2.0 * s, rw + 2.0, half), TEXT_DIM);
            let sub = if hit.snippet.is_empty() { hit.cwd.clone() } else { hit.snippet.clone() };
            g.text(&sub, &g.ui_small, Rect::new(r.x + 10.0 * s, r.y + half - 1.0 * s, r.w - 20.0 * s, half), TEXT_DIM);
            f.rows.push((r, i));
        }
        let fy = card.bottom() - foot_h;
        g.fill(Rect::new(card.x, fy, card.w, 1.0), BORDER);
        let tip = "Enter open · ↑↓ move · Esc close · it opens asleep; press Enter in the tab to continue";
        g.text(tip, &g.ui_small, Rect::new(card.x + pad, fy, card.w - 2.0 * pad, foot_h), TEXT_DIM);
        self.ui.finder = Some(f);
    }
}

/// The index, shared with its background reader.
pub fn new_index() -> Shared {
    Arc::new(Mutex::new(Index::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(title: &str, prompts: &str, answers: &str, modified: u64) -> ChatEntry {
        let mut c = ChatEntry { session: title.into(), title: title.into(), prompts: prompts.into(), answers: answers.into(), modified, ..Default::default() };
        c.update_lower();
        c
    }

    #[test]
    fn reads_titles_and_messages_from_log_lines() {
        let mut e = ChatEntry::default();
        add_line(&mut e, br#"{"type":"user","cwd":"A:\\Projects\\x","message":{"role":"user","content":"Fix the WhatsApp webhook"}}"#);
        add_line(&mut e, br#"{"type":"user","isMeta":true,"message":{"content":"Caveat: ignore"}}"#);
        add_line(&mut e, br#"{"type":"user","message":{"content":[{"type":"text","text":"<command-name>/clear</command-name>"},{"type":"tool_result","content":"x"}]}}"#);
        add_line(&mut e, br#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done: the webhook now retries."},{"type":"tool_use","name":"Bash"}]}}"#);
        add_line(&mut e, br#"{"type":"ai-title","aiTitle":"WhatsApp webhook fix","sessionId":"s"}"#);
        assert_eq!(e.cwd, "A:\\Projects\\x");
        assert_eq!(e.first_prompt, "Fix the WhatsApp webhook");
        assert_eq!(e.prompts, "Fix the WhatsApp webhook\n");
        assert_eq!(e.answers, "Done: the webhook now retries.\n");
        assert_eq!(e.title, "WhatsApp webhook fix");
        // Your own rename wins over Claude's title.
        add_line(&mut e, br#"{"type":"custom-title","customTitle":"Webhook"}"#);
        add_line(&mut e, br#"{"type":"ai-title","aiTitle":"Other"}"#);
        assert_eq!(e.title, "Webhook");
    }

    #[test]
    fn search_needs_every_word_and_ranks_titles_first() {
        let chats = vec![
            chat("Interview prep", "explain rag and webhook retries", "", 100),
            chat("WhatsApp webhook", "", "", 50),
            chat("Grading", "nothing", "a webhook was mentioned", 200),
        ];
        let hits = search(&chats, "Webhook");
        assert_eq!(hits.iter().map(|h| h.title.as_str()).collect::<Vec<_>>(), ["WhatsApp webhook", "Interview prep", "Grading"]);
        assert!(hits[1].snippet.contains("webhook retries"));
        assert_eq!(search(&chats, "webhook rag").len(), 1);
        // No words: newest first.
        assert_eq!(search(&chats, "  ")[0].title, "Grading");
    }

    #[test]
    fn keeps_only_the_newest_text() {
        let mut s = "é".repeat(KEEP);
        keep_tail(&mut s);
        assert!(s.len() <= KEEP && s.chars().all(|c| c == 'é'));
    }

    #[test]
    fn times() {
        assert_eq!(ago(1000, 1030), "just now");
        assert_eq!(ago(1000, 1000 + 7200), "2 h ago");
        // 2026-11-18 00:00 UTC
        assert_eq!(ago(1_794_960_000, 1_794_960_000 + 30 * 86_400), "18 Nov");
    }
}
