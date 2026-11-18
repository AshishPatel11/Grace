//! Claude's chat, drawn more like a chat app.
//!
//! Claude Code already turns its markdown into terminal text (bold, colours, tables), but a
//! terminal has no boxes, buttons or bigger letters. Grace adds those on top. The characters
//! themselves never change (selecting and copying text works as before):
//!
//! - **Code blocks** get a box with the language name and a Copy button. Claude prints code
//!   as plain coloured text (a ```bash block often has no colour at all), so the screen alone
//!   cannot tell code from a normal sentence. Grace reads the code blocks from Claude's chat
//!   log and looks for those exact lines on screen. Copy gives the exact code from the log.
//! - **Tool calls** ("● Update(...)", "⎿ ...", "Ran 3 shell commands") get dimmer text and a
//!   thin line on the left in the call's colour (green = done, red = failed), so Claude's
//!   answer is what stands out.
//! - **Markdown**: headings are bigger, "---" becomes a real line, "- " becomes "•", and
//!   `inline code` gets a soft background.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::Flags;

use crate::gfx::{Gfx, Rgb};
use crate::layout::Rect;
use crate::render::VCell;
use crate::theme::*;

// ---------------- code blocks from the chat log ----------------

/// A big chat log is only read from this far before its end the first time (recent
/// messages are the ones on screen). 93 MB logs exist.
const TAIL_BYTES: u64 = 8 << 20;
/// Screen rows are first matched to code lines by their first characters (cheap).
const KEY_CHARS: usize = 16;
/// Code blocks kept per chat (the newest ones).
const MAX_BLOCKS: usize = 600;

pub struct CodeBlock {
    pub lang: String,
    /// Each line with its spaces squeezed: this is how it is compared with the screen.
    norm: Vec<String>,
    /// The exact code, for the Copy button.
    pub code: Arc<str>,
}

/// The code blocks Claude wrote in one chat, read from its log as it grows.
pub struct CodeIndex {
    session: String,
    path: Option<PathBuf>,
    looked: Option<Instant>,
    scanned: u64,
    checked: Option<Instant>,
    blocks: Vec<CodeBlock>,
    /// First characters of a code line -> (block, line).
    keys: HashMap<String, Vec<(u32, u32)>>,
}

impl CodeIndex {
    pub fn new(session: &str) -> CodeIndex {
        CodeIndex {
            session: session.to_string(),
            path: None,
            looked: None,
            scanned: 0,
            checked: None,
            blocks: Vec::new(),
            keys: HashMap::new(),
        }
    }

    /// Read what Claude added to the log since last time. Checks at most about once a
    /// second, so calling it on every frame is fine.
    pub fn update(&mut self) {
        if self.checked.is_some_and(|t| t.elapsed() < Duration::from_millis(800)) {
            return;
        }
        self.checked = Some(Instant::now());
        if self.path.is_none() {
            // A new chat has no log for a moment. Look again every few seconds.
            if self.looked.is_some_and(|t| t.elapsed() < Duration::from_secs(4)) {
                return;
            }
            self.looked = Some(Instant::now());
            self.path = crate::images::transcript_path(&self.session);
        }
        let Some(path) = &self.path else { return };
        let Ok(mut f) = std::fs::File::open(path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.scanned {
            // The log was rewritten: start again.
            self.scanned = 0;
            self.blocks.clear();
        }
        if len == self.scanned {
            return;
        }
        let mut start = self.scanned;
        let mid_line = start == 0 && len > TAIL_BYTES;
        if mid_line {
            start = len - TAIL_BYTES;
        }
        if f.seek(SeekFrom::Start(start)).is_err() {
            return;
        }
        let mut buf = Vec::with_capacity((len - start) as usize);
        if f.take(len - start).read_to_end(&mut buf).is_err() {
            return;
        }
        let mut pos = 0usize;
        if mid_line {
            // We started inside a line: skip to the next one.
            match buf.iter().position(|b| *b == b'\n') {
                Some(i) => pos = i + 1,
                None => return,
            }
        }
        // Only whole lines; a line still being written is read next time.
        let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') else { return };
        let before = self.blocks.len();
        while pos <= last_nl {
            let end = pos + buf[pos..=last_nl].iter().position(|b| *b == b'\n').unwrap_or(last_nl - pos);
            let line = &buf[pos..end];
            if has(line, b"\"assistant\"") && (has(line, b"```") || has(line, b"~~~")) {
                self.add_message(line);
            }
            pos = end + 1;
        }
        self.scanned = start + last_nl as u64 + 1;
        if self.blocks.len() != before {
            if self.blocks.len() > MAX_BLOCKS {
                self.blocks.drain(..self.blocks.len() - MAX_BLOCKS);
            }
            self.rebuild_keys();
        }
    }

    fn add_message(&mut self, line: &[u8]) {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else { return };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            return;
        }
        let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) else { return };
        for b in content {
            if b.get("type").and_then(|t| t.as_str()) != Some("text") {
                continue;
            }
            let Some(text) = b.get("text").and_then(|t| t.as_str()) else { continue };
            self.add_text(text);
        }
    }

    fn add_text(&mut self, text: &str) {
        for (lang, lines) in fences(text) {
            if lines.iter().all(|l| l.trim().is_empty()) {
                continue;
            }
            let code: Arc<str> = lines.join("\n").into();
            let norm = lines.iter().map(|l| squeeze(l)).collect();
            self.blocks.push(CodeBlock { lang, norm, code });
        }
    }

    fn rebuild_keys(&mut self) {
        self.keys.clear();
        for (b, blk) in self.blocks.iter().enumerate() {
            for (i, l) in blk.norm.iter().enumerate() {
                if !l.is_empty() {
                    self.keys.entry(key_of(l)).or_default().push((b as u32, i as u32));
                }
            }
        }
    }

    #[cfg(test)]
    fn from_markdown(md: &str) -> CodeIndex {
        let mut i = CodeIndex::new("test");
        i.add_text(md);
        i.rebuild_keys();
        i
    }
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn key_of(s: &str) -> String {
    s.chars().take(KEY_CHARS).collect()
}

/// All runs of spaces become one space, and the ends are trimmed. Claude may draw a tab
/// as spaces, and a long line wraps onto the next row, so exact spaces cannot be compared.
pub fn squeeze(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// The fenced code blocks (```lang ... ```) in a markdown text: (language, lines).
/// A block that is not closed yet (the message was cut) is left out.
pub fn fences(md: &str) -> Vec<(String, Vec<String>)> {
    struct Open {
        ch: char,
        len: usize,
        indent: usize,
        lang: String,
        lines: Vec<String>,
    }
    let mut out = Vec::new();
    let mut open: Option<Open> = None;
    for raw in md.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let indent = line.len() - line.trim_start_matches(' ').len();
        let t = &line[indent..];
        if let Some(o) = &mut open {
            let run = t.chars().take_while(|c| *c == o.ch).count();
            if run >= o.len && t[run..].trim().is_empty() {
                let o = open.take().unwrap();
                out.push((o.lang, o.lines));
            } else {
                // Lines lose the fence's own indent (a code block inside a list item).
                o.lines.push(line[indent.min(o.indent)..].to_string());
            }
            continue;
        }
        for ch in ['`', '~'] {
            let run = t.chars().take_while(|c| *c == ch).count();
            if run >= 3 {
                let info = t[run..].trim();
                if ch == '`' && info.contains('`') {
                    break; // ```x``` on one line is inline code, not a block
                }
                let lang = info.split_whitespace().next().unwrap_or("").to_string();
                open = Some(Open { ch, len: run, indent, lang, lines: Vec::new() });
                break;
            }
        }
    }
    out
}

// ---------------- what each screen row is ----------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// Above the first marker on screen, and not seen before.
    Unknown,
    /// Claude's answer (after a white "●").
    Prose,
    /// A tool call and its result, in the colour of its "●".
    Tool(Rgb),
    /// Your own message ("❯" with a grey background).
    User,
    /// Anything else in the chat ("✻ Cooked for 7s", boxes).
    Other,
    /// Claude's input box and status lines at the bottom.
    Chrome,
}

pub struct Card {
    pub top: usize,
    pub bottom: usize,
    /// The block goes on above / below the screen.
    pub cut_top: bool,
    pub cut_bottom: bool,
    /// Column of the leftmost code character.
    pub left: usize,
    /// The top row ends early enough for a floating Copy button (used when `cut_top`).
    pub room_top: bool,
    /// The next code box starts right after the empty row under this one.
    pub next_close: bool,
    pub lang: String,
    pub code: Arc<str>,
}

pub struct Heading {
    pub row: usize,
    pub col: usize,
    pub text: String,
    /// 1 for "# Title", 2 for "## Title" and smaller (Claude draws those the same way).
    pub level: u8,
    /// It fits on its row when drawn bigger (set while drawing).
    pub big: bool,
}

#[derive(Default)]
pub struct Deco {
    pub cards: Vec<Card>,
    /// Tool rows: (first row, last row, colour, the first row holds the "●").
    pub rails: Vec<(usize, usize, Rgb, bool)>,
    /// "---" rows: (row, column).
    pub rules: Vec<(usize, usize)>,
    pub headings: Vec<Heading>,
    /// Inline code: (row, first column, last column).
    pub pills: Vec<(usize, usize, usize)>,
    /// Rows whose own text is not drawn (a line or a big heading is drawn instead).
    pub hide: Vec<bool>,
}

/// The colour Claude gives inline code and plain code blocks, per Claude theme
/// (dark, light, and their colour-blind versions).
fn is_code_color(c: Rgb) -> bool {
    matches!((c.0, c.1, c.2), (177, 185, 249) | (87, 105, 247) | (153, 204, 255) | (51, 102, 255))
}

/// White or black: the "●" of Claude's own text. Green, red or grey = a tool call.
fn is_text_dot(c: Rgb) -> bool {
    let (mx, mn) = (c.0.max(c.1).max(c.2), c.0.min(c.1).min(c.2));
    mx - mn < 24 && (mn > 0xD8 || mx < 0x30)
}

/// Claude's grey for "Ran 3 shell commands" and "⎿" (not the DIM grey of a quote bar).
fn is_grey(cell: &VCell) -> bool {
    let c = cell.fg;
    let (mx, mn) = (c.0.max(c.1).max(c.2), c.0.min(c.1).min(c.2));
    !cell.flags.contains(Flags::DIM) && mx - mn < 16 && (0x70..=0xB8).contains(&mn)
}

/// The first row of Claude's input box and status lines (they never scroll). The box sits
/// between two full-width lines: everything from the second-to-last such line down.
pub fn chrome_from(rows: &[(u64, Line, Vec<VCell>)]) -> usize {
    let rule_rows: Vec<usize> = (0..rows.len())
        .filter(|&r| {
            let cells = &rows[r].2;
            cells.first().is_some_and(|c| c.c == '─') && cells.iter().filter(|c| c.c == '─').count() * 10 >= cells.len() * 8
        })
        .collect();
    match rule_rows.len() {
        0 => rows.len(),
        1 => rule_rows[0],
        k => rule_rows[k - 2],
    }
}

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x100000001b3)
}

/// Look at the rows on screen: decide what each one is, find the code blocks, and change
/// the cells that are drawn differently (dimmer tool rows, "•" bullets). Rows that change
/// get a new hash, so the shaped-text cache keeps both versions apart.
pub fn analyze(rows: &mut [(u64, Line, Vec<VCell>)], idx: Option<&CodeIndex>, seen: &mut HashMap<u64, Kind>) -> Deco {
    let n = rows.len();
    let mut d = Deco { hide: vec![false; n], ..Default::default() };
    if n == 0 {
        return d;
    }
    // Text of each row (wide characters once), squeezed, and where it starts.
    let mut texts = Vec::with_capacity(n);
    let mut first = Vec::with_capacity(n);
    let mut last = Vec::with_capacity(n);
    for (_, _, cells) in rows.iter() {
        let f = cells.iter().position(|c| !c.c.is_whitespace());
        // Claude's floating "Jump to bottom" bar sits in the middle of an empty row.
        if f.is_some_and(|f| f > 8 && cells[f].bg != BG) {
            texts.push(String::new());
            first.push(None);
            last.push(None);
            continue;
        }
        let s: String = cells.iter().filter(|c| !c.flags.contains(Flags::WIDE_CHAR_SPACER)).map(|c| c.c).collect();
        texts.push(squeeze(&s));
        first.push(f);
        last.push(cells.iter().rposition(|c| !c.c.is_whitespace()));
    }
    let blank = |r: usize| texts[r].is_empty();

    // Claude's input box sits between two full-width lines; everything from the
    // second-to-last such line down is the box and the status lines.
    let chrome_from = chrome_from(rows);

    // 1) What each row is. A marker in column 0 starts a new part of the chat;
    //    rows under it belong to it.
    let mut kind = vec![Kind::Unknown; n];
    // Rows that start a new part (a marker, or a tool summary line).
    let mut starts = vec![false; n];
    let mut cur = Kind::Unknown;
    for r in 0..n {
        if r >= chrome_from {
            kind[r] = Kind::Chrome;
            continue;
        }
        let cells = &rows[r].2;
        let Some(c0) = cells.first() else { continue };
        if c0.c == '●' || c0.c == '⏺' {
            cur = if is_text_dot(c0.fg) { Kind::Prose } else { Kind::Tool(c0.fg) };
            starts[r] = true;
        } else if c0.c == '❯' {
            cur = Kind::User;
        } else if cur == Kind::User && c0.bg == BG {
            // Your message has a grey background on every row; it ended. (While you scroll
            // up, Claude pins your question on the top row, over the middle of its answer.)
            cur = Kind::Unknown;
            if let Some(k) = seen.get(&rows[r].0).filter(|_| !blank(r)) {
                cur = *k;
            }
        } else if c0.c != ' ' {
            cur = Kind::Other;
        } else if first[r] == Some(2) && (r == 0 || blank(r - 1)) && is_grey(&cells[2]) {
            // "Ran 1 shell command", "Read 2 files" (a tool summary), or a "⎿" result.
            // "· 1 failed" is in red: then the rail is red too.
            let red = cells.iter().map(|c| c.fg).find(|c| c.0 > 0xC0 && c.1 < 0x90 && c.2 < 0xA0);
            cur = Kind::Tool(red.unwrap_or(cells[2].fg));
            starts[r] = true;
        } else if cur == Kind::Unknown && !blank(r) {
            // Top of the screen: maybe this row was seen before, with its marker.
            if let Some(k) = seen.get(&rows[r].0) {
                cur = *k;
            }
        }
        kind[r] = cur;
    }
    if seen.len() > 4000 {
        seen.clear();
    }
    for r in 0..n {
        if !blank(r) && matches!(kind[r], Kind::Prose | Kind::Tool(_)) {
            seen.insert(rows[r].0, kind[r]);
        }
    }

    // 2) Code blocks, only where Claude's answer can be.
    let ok: Vec<bool> = kind.iter().map(|k| matches!(k, Kind::Prose | Kind::Unknown)).collect();
    if let Some(idx) = idx.filter(|i| !i.blocks.is_empty()) {
        d.cards = match_cards(&texts, &ok, idx);
        for c in &mut d.cards {
            c.left = (c.top..=c.bottom).filter_map(|r| first[r]).min().unwrap_or(2).max(1);
            let cols = rows[c.top].2.len();
            c.room_top = last[c.top].map_or(true, |l| l + 14 < cols);
        }
    }
    for k in 1..d.cards.len() {
        if d.cards[k].top == d.cards[k - 1].bottom + 2 {
            d.cards[k - 1].next_close = true;
        }
    }
    let mut in_card = vec![false; n];
    for c in &d.cards {
        in_card[c.top..=c.bottom].iter_mut().for_each(|x| *x = true);
    }

    // 3) Change cells, and note headings, lines and inline code.
    for r in 0..n {
        let mut changed = 0u64;
        match kind[r] {
            Kind::Tool(_) => {
                // Dimmer text; the "●" keeps its colour.
                for (i, c) in rows[r].2.iter_mut().enumerate() {
                    if i > 0 || c.c != '●' {
                        c.fg = c.fg.mix(c.bg, 0.32);
                    }
                }
                changed = 1;
            }
            Kind::Prose | Kind::Unknown if !in_card[r] && !blank(r) => {
                let cells = &mut rows[r].2;
                let f = first[r].unwrap_or(0);
                let marker = cells[0].c != ' ';
                if texts[r] == "---" && f >= 2 {
                    d.rules.push((r, f));
                    d.hide[r] = true;
                } else if !marker
                    && f >= 2
                    && r > 0
                    && blank(r - 1)
                    && (r + 1 == n || blank(r + 1))
                    && cells.iter().all(|c| c.c == ' ' || c.flags.contains(Flags::BOLD))
                {
                    let h1 = cells[f].flags.contains(Flags::ITALIC) && cells[f].flags.intersects(Flags::ALL_UNDERLINES);
                    let text: String = cells[f..].iter().filter(|c| !c.flags.contains(Flags::WIDE_CHAR_SPACER)).map(|c| c.c).collect();
                    d.headings.push(Heading { row: r, col: f, text: text.trim_end().to_string(), level: if h1 { 1 } else { 2 }, big: false });
                } else {
                    // "- item" -> "• item"; deeper lists get "◦".
                    if f + 1 < cells.len() && cells[f].c == '-' && cells[f + 1].c == ' ' && f >= 2 && !marker {
                        cells[f].c = if f <= 3 { '•' } else { '◦' };
                        changed = 2;
                    }
                    // Inline code: a soft background behind Claude's code colour. A whole
                    // row in that colour is a code block that was not found in the log.
                    let mut c = 0;
                    while c < cells.len() {
                        if !is_code_color(cells[c].fg) {
                            c += 1;
                            continue;
                        }
                        let s = c;
                        while c < cells.len() && is_code_color(cells[c].fg) {
                            c += 1;
                        }
                        let (mut a, mut b) = (s, c - 1);
                        while a < b && cells[a].c == ' ' {
                            a += 1;
                        }
                        while b > a && cells[b].c == ' ' {
                            b -= 1;
                        }
                        let whole = Some(a) == first[r] && Some(b) == last[r];
                        if cells[a].c != ' ' && !whole {
                            d.pills.push((r, a, b));
                        }
                    }
                }
            }
            _ => {}
        }
        if changed != 0 {
            rows[r].0 = mix(rows[r].0, 0x9E37_79B9_7F4A_7C15 ^ changed);
        }
    }

    // 4) Rails beside tool calls, from under the "●" to the last row of the call.
    let mut r = 0;
    while r < n {
        let Kind::Tool(c) = kind[r] else {
            r += 1;
            continue;
        };
        let a = r;
        r += 1;
        while r < n && kind[r] == Kind::Tool(c) && !starts[r] {
            r += 1;
        }
        let mut b = r - 1;
        while b > a && blank(b) {
            b -= 1;
        }
        let dot = rows[a].2.first().is_some_and(|x| x.c == '●' || x.c == '⏺');
        d.rails.push((a, b, c, dot));
    }
    d
}

/// Find code blocks from the log on screen. A block counts when all its lines are there in
/// order (a long line may wrap onto more rows), with an empty row or the screen's edge
/// before and after it. Only the edge of the screen may cut a block.
fn match_cards(texts: &[String], ok: &[bool], idx: &CodeIndex) -> Vec<Card> {
    let n = texts.len();
    let mut out: Vec<Card> = Vec::new();
    let mut r = 0;
    while r < n {
        if !ok[r] || texts[r].is_empty() {
            r += 1;
            continue;
        }
        // (last row, cut at the bottom, block, first line on screen)
        let mut best: Option<(usize, bool, usize, usize)> = None;
        if let Some(cands) = idx.keys.get(&key_of(&texts[r])) {
            // Newest blocks first: the same code shown twice gets the newest language name.
            for &(b, i) in cands.iter().rev() {
                let (b, i) = (b as usize, i as usize);
                // The block must start here, or go on above the screen.
                if i > 0 && r > 0 {
                    continue;
                }
                if r > 0 && !texts[r - 1].is_empty() {
                    continue;
                }
                if let Some((end, cut)) = walk(texts, ok, r, &idx.blocks[b].norm[i..]) {
                    if best.is_none_or(|x| end > x.0) {
                        best = Some((end, cut, b, i));
                    }
                }
            }
        }
        match best {
            Some((end, cut, b, i)) => {
                let blk = &idx.blocks[b];
                out.push(Card {
                    top: r,
                    bottom: end,
                    // On the top row there is no empty row above for the header.
                    cut_top: i > 0 || r == 0,
                    cut_bottom: cut,
                    left: 2,
                    room_top: true,
                    next_close: false,
                    lang: blk.lang.clone(),
                    code: blk.code.clone(),
                });
                r = end + 1;
            }
            None => r += 1,
        }
    }
    out
}

/// Match `lines` against the rows from `r` on. Returns the last row used, and whether the
/// screen (or Claude's input box) cut the block before its end.
fn walk(texts: &[String], ok: &[bool], r: usize, lines: &[String]) -> Option<(usize, bool)> {
    let n = texts.len();
    let mut row = r;
    for want in lines {
        if row >= n || !ok[row] {
            return (row > r).then(|| (row - 1, true));
        }
        if want.is_empty() {
            if !texts[row].is_empty() {
                return None;
            }
            row += 1;
            continue;
        }
        let mut rest: &str = want;
        loop {
            let got = texts[row].as_str();
            if got.is_empty() {
                return None;
            }
            if rest == got {
                row += 1;
                break;
            }
            if got.len() < rest.len() && rest.starts_with(got) {
                // A long line that wrapped: the next row goes on with the rest of it.
                rest = rest[got.len()..].trim_start();
                row += 1;
                if row >= n || !ok[row] {
                    return Some((row - 1, true));
                }
                continue;
            }
            return None;
        }
    }
    // After the block: an empty row, or the end of the screen.
    if row < n && ok[row] && !texts[row].is_empty() {
        return None;
    }
    Some((row - 1, false))
}

// ---------------- drawing ----------------

/// A Copy button drawn in the last frame.
#[derive(Clone)]
pub struct CodeHit {
    pub rect: Rect,
    pub code: Arc<str>,
}

impl CodeHit {
    pub fn key(&self) -> usize {
        Arc::as_ptr(&self.code) as *const u8 as usize
    }
}

pub struct Geo {
    pub ox: f32,
    pub oy: f32,
    pub cw: f32,
    pub ch: f32,
    pub cols: usize,
    pub s: f32,
}

impl Geo {
    fn y(&self, r: usize) -> f32 {
        self.oy + r as f32 * self.ch
    }
    fn x(&self, c: usize) -> f32 {
        self.ox + c as f32 * self.cw
    }
    fn card_rect(&self, c: &Card) -> Rect {
        let x0 = self.x(c.left - 1);
        let x1 = self.x(self.cols.saturating_sub(1)).max(x0 + 10.0 * self.cw);
        let top = if c.cut_top { self.y(c.top) } else { self.y(c.top - 1) + self.ch * 0.12 };
        // Some space under the code, unless the next box's header needs that empty row.
        let pad = if c.cut_bottom || c.next_close { 0.0 } else { self.ch * 0.4 };
        Rect::new(x0, top, x1 - x0, self.y(c.bottom + 1) + pad - top)
    }
}

/// Backgrounds, drawn before the text. Also decides which headings fit drawn bigger
/// (their own small text is then not drawn).
pub fn paint_under(g: &Gfx, d: &mut Deco, geo: &Geo) {
    for h in &mut d.headings {
        let fmt = &g.heads[if h.level == 1 { 0 } else { 1 }];
        h.big = g.measure(&h.text, fmt) <= geo.x(geo.cols.saturating_sub(1)) - geo.x(h.col);
        d.hide[h.row] |= h.big;
    }
    let rad = 6.0 * geo.s;
    for c in &d.cards {
        let r = geo.card_rect(c);
        g.round(r, rad, CODE_BG, 1.0);
        if !c.cut_top {
            // Header strip (language name and Copy) in the empty row above the code.
            let hh = geo.y(c.top) - r.y;
            g.round(Rect::new(r.x, r.y, r.w, hh + rad), rad, CODE_HEAD, 1.0);
            g.fill(Rect::new(r.x, r.y + hh, r.w, rad), CODE_BG);
            g.fill(Rect::new(r.x, r.y + hh, r.w, geo.s.round().max(1.0)), BORDER);
        }
        g.outline(r, rad, BORDER, geo.s.round().max(1.0));
    }
    for &(row, a, b) in &d.pills {
        let y = geo.y(row);
        let p = 2.0 * geo.s;
        g.round(Rect::new(geo.x(a) - p, y + geo.s, (b + 1 - a) as f32 * geo.cw + 2.0 * p, geo.ch - 2.0 * geo.s), 3.0 * geo.s, PILL, 1.0);
    }
}

/// Lines, big headings, rails and Copy buttons, drawn over the text.
/// `mouse`: for hover. `copied`: the button that was just clicked.
pub fn paint_over(g: &Gfx, d: &mut Deco, geo: &Geo, mouse: (f32, f32), copied: Option<usize>, hits: &mut Vec<CodeHit>) {
    let th = geo.s.round().max(1.0);
    for &(row, col) in &d.rules {
        let y = (geo.y(row) + geo.ch / 2.0).round();
        g.fill(Rect::new(geo.x(col), y, geo.x(geo.cols.saturating_sub(1)) - geo.x(col), th), RULE);
    }
    for h in d.headings.iter().filter(|h| h.big) {
        let fmt = &g.heads[if h.level == 1 { 0 } else { 1 }];
        let room = geo.x(geo.cols.saturating_sub(1)) - geo.x(h.col);
        let y = geo.y(h.row);
        {
            // Bigger text, centred on its row; the empty rows around it give it space.
            g.text(&h.text, fmt, Rect::new(geo.x(h.col), y - geo.ch, room, geo.ch * 3.0), HEAD_TEXT);
            if h.level == 1 {
                let ly = (y + geo.ch * 1.55).round();
                g.fill(Rect::new(geo.x(h.col), ly, room, th), RULE);
            }
        }
    }
    let w = (2.0 * geo.s).round().max(2.0);
    for &(a, b, c, dot) in &d.rails {
        let y0 = if dot { geo.y(a) + geo.ch * 0.9 } else { geo.y(a) + geo.ch * 0.15 };
        let y1 = geo.y(b + 1) - geo.ch * 0.15;
        if y1 > y0 {
            let x = (geo.ox + geo.cw * 0.5 - w / 2.0).round();
            g.round(Rect::new(x, y0, w, y1 - y0), w / 2.0, c.mix(BG, 0.45), 1.0);
        }
    }
    for c in &d.cards {
        let r = geo.card_rect(c);
        let label_h = geo.ch;
        let (bar_y, bar_h) = if c.cut_top {
            if !c.room_top {
                continue;
            }
            (geo.y(c.top), label_h)
        } else {
            (r.y, geo.y(c.top) - r.y)
        };
        if !c.cut_top {
            let lang = if c.lang.is_empty() { "code" } else { c.lang.as_str() };
            g.text(lang, &g.ui_small, Rect::new(r.x + geo.cw, bar_y, r.w / 2.0, bar_h), TEXT_DIM.mix(TEXT, 0.35));
        }
        let hit = CodeHit { rect: Rect::default(), code: c.code.clone() };
        let done = copied == Some(hit.key());
        let label = if done { "Copied" } else { "Copy" };
        let tw = g.measure(label, &g.ui_small);
        let icon = 11.0 * geo.s;
        let bw = (tw + icon + 18.0 * geo.s).round();
        let bh = (bar_h - 4.0 * geo.s).max(12.0 * geo.s).min(22.0 * geo.s);
        let b = Rect::new(r.right() - bw - 5.0 * geo.s, bar_y + (bar_h - bh) / 2.0, bw, bh);
        // The button is small; the area that takes the click is a bit bigger.
        let grab = Rect::new(b.x - 4.0 * geo.s, b.y - 5.0 * geo.s, b.w + 8.0 * geo.s, b.h + 10.0 * geo.s);
        let hot = grab.contains(mouse.0, mouse.1);
        if c.cut_top {
            g.round(b, 4.0 * geo.s, CODE_HEAD, 1.0);
        }
        if hot {
            g.round(b, 4.0 * geo.s, TAB_HOVER.mix(TEXT, 0.08), 1.0);
        }
        let col = if done { GREEN } else if hot { TEXT } else { TEXT_DIM };
        // Two small overlapping squares: the usual copy picture.
        let ix = b.x + 7.0 * geo.s;
        let iy = b.y + (bh - icon) / 2.0;
        let sq = icon * 0.72;
        if done {
            g.line(ix + icon * 0.1, iy + icon * 0.55, ix + icon * 0.4, iy + icon * 0.85, 1.5 * geo.s, col);
            g.line(ix + icon * 0.4, iy + icon * 0.85, ix + icon * 0.95, iy + icon * 0.2, 1.5 * geo.s, col);
        } else {
            g.outline(Rect::new(ix + icon - sq, iy, sq, sq), 2.0 * geo.s, col, th);
            g.round(Rect::new(ix, iy + icon - sq, sq, sq), 2.0 * geo.s, CODE_HEAD, 1.0);
            g.outline(Rect::new(ix, iy + icon - sq, sq, sq), 2.0 * geo.s, col, th);
        }
        g.text(label, &g.ui_small, Rect::new(ix + icon + 5.0 * geo.s, b.y, tw + 4.0, bh), col);
        hits.push(CodeHit { rect: grab, ..hit });
    }
}

impl Deco {
    /// For the test pipe: what was found on screen.
    pub fn summary(&self, rows: &[(u64, Line, Vec<VCell>)]) -> serde_json::Value {
        let text = |r: usize| -> String { rows[r].2.iter().map(|c| c.c).collect::<String>().trim().to_string() };
        serde_json::json!({
            "cards": self.cards.iter().map(|c| serde_json::json!({"top": c.top, "bottom": c.bottom, "lang": c.lang,
                "cut_top": c.cut_top, "cut_bottom": c.cut_bottom, "first": text(c.top)})).collect::<Vec<_>>(),
            "rails": self.rails.iter().map(|r| serde_json::json!([r.0, r.1, format!("{:02x}{:02x}{:02x}", r.2.0, r.2.1, r.2.2), text(r.0)])).collect::<Vec<_>>(),
            "headings": self.headings.iter().map(|h| serde_json::json!([h.row, h.level, h.text])).collect::<Vec<_>>(),
            "rules": self.rules.iter().map(|r| r.0).collect::<Vec<_>>(),
            "pills": self.pills.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(c: char, fg: Rgb, flags: Flags) -> VCell {
        VCell { c, zw: None, fg, bg: BG, flags, sel: false }
    }

    /// A screen row: text in one colour, padded to 60 columns.
    fn row(s: &str, fg: Rgb, flags: Flags) -> (u64, Line, Vec<VCell>) {
        let mut cells: Vec<VCell> = s.chars().map(|c| cell(c, fg, flags)).collect();
        while cells.len() < 60 {
            cells.push(cell(' ', FG, Flags::empty()));
        }
        let h = s.bytes().fold(7u64, |h, b| mix(h, b as u64)) ^ fg.0 as u64;
        (h, Line(0), cells)
    }

    fn plain(s: &str) -> (u64, Line, Vec<VCell>) {
        row(s, FG, Flags::empty())
    }

    /// "● text": the dot in `dot`, the rest plain.
    fn dot(dot: Rgb, s: &str) -> (u64, Line, Vec<VCell>) {
        let mut r = plain(&format!("● {s}"));
        r.2[0].fg = dot;
        r
    }

    const WHITE: Rgb = Rgb(255, 255, 255);
    const OK: Rgb = Rgb(0x4e, 0xba, 0x65);

    #[test]
    fn fences_are_found() {
        let md = "Run this:\n\n```bash\ncargo build --release\n```\n\nand\n\n  ~~~\n  a\n    b\n  ~~~\n```rust\nnot closed";
        let f = fences(md);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0], ("bash".to_string(), vec!["cargo build --release".to_string()]));
        assert_eq!(f[1], (String::new(), vec!["a".to_string(), "  b".to_string()]));
        assert!(fences("use ```x``` inline").is_empty());
    }

    #[test]
    fn a_plain_code_line_is_found_from_the_log() {
        // Claude draws a ```bash block in the same white as a sentence.
        let idx = CodeIndex::from_markdown("Build it:\n\n```bash\ncargo build --release\n```\n");
        let mut rows = vec![dot(WHITE, "Build it:"), plain(""), plain("  cargo build --release"), plain(""), plain("  Done.")];
        let d = analyze(&mut rows, Some(&idx), &mut HashMap::new());
        assert_eq!(d.cards.len(), 1);
        let c = &d.cards[0];
        assert_eq!((c.top, c.bottom, c.cut_top, c.cut_bottom), (2, 2, false, false));
        assert_eq!((c.lang.as_str(), &*c.code), ("bash", "cargo build --release"));
    }

    #[test]
    fn the_jump_to_bottom_bar_counts_as_an_empty_row() {
        let idx = CodeIndex::from_markdown("```python
print(1)
```");
        let mut bar = plain("                    Jump to bottom (ctrl+End)");
        bar.2[20..45].iter_mut().for_each(|c| c.bg = Rgb(0x37, 0x37, 0x37));
        let mut rows = vec![plain(""), plain("  print(1)"), bar];
        assert_eq!(analyze(&mut rows, Some(&idx), &mut HashMap::new()).cards.len(), 1);
    }

    #[test]
    fn the_same_words_in_a_sentence_are_not_code() {
        let idx = CodeIndex::from_markdown("```\nhello world\n```");
        // Not alone between empty rows: part of a paragraph.
        let mut rows = vec![dot(WHITE, "intro"), plain("  hello world"), plain("  more text"), plain("")];
        assert!(analyze(&mut rows, Some(&idx), &mut HashMap::new()).cards.is_empty());
    }

    #[test]
    fn blocks_cut_by_the_screen_and_wrapped_lines() {
        let long = "let x = some_function_name(argument_one, argument_two, argument_three);";
        let idx = CodeIndex::from_markdown(&format!("```rust\nfn main() {{\n    {long}\n\n    done();\n}}\n```"));
        // The screen starts in the middle of the block, and the long line wraps.
        let (a, b) = long.split_at(40);
        let mut rows = vec![plain(&format!("      {a}")), plain(&format!("  {b}")), plain(""), plain("      done();"), plain("  }"), plain("")];
        let d = analyze(&mut rows, Some(&idx), &mut HashMap::new());
        assert_eq!(d.cards.len(), 1, "{:?}", d.cards.iter().map(|c| (c.top, c.bottom)).collect::<Vec<_>>());
        let c = &d.cards[0];
        assert_eq!((c.top, c.bottom, c.cut_top, c.cut_bottom), (0, 4, true, false));
        // And cut at the bottom.
        let mut rows = vec![plain(""), plain("  fn main() {"), plain(&format!("      {a}"))];
        let d = analyze(&mut rows, Some(&idx), &mut HashMap::new());
        assert_eq!((d.cards[0].top, d.cards[0].bottom, d.cards[0].cut_bottom), (1, 2, true));
    }

    #[test]
    fn tool_rows_are_dimmed_and_get_a_rail() {
        let mut rows = vec![
            dot(OK, "Write(demo.txt)"),
            plain("  ⎿  Wrote 3 lines"),
            plain(""),
            row("  Ran 1 shell command", Rgb(0x99, 0x99, 0x99), Flags::empty()),
            plain(""),
            dot(WHITE, "Here is the answer."),
            plain("  More answer."),
        ];
        let before = rows[1].0;
        let d = analyze(&mut rows, None, &mut HashMap::new());
        assert_eq!(d.rails.len(), 2);
        assert_eq!((d.rails[0].0, d.rails[0].1, d.rails[0].3), (0, 1, true));
        assert_eq!((d.rails[1].0, d.rails[1].1, d.rails[1].3), (3, 3, false));
        assert_ne!(rows[1].0, before, "a changed row gets a new hash");
        assert_eq!(rows[0].2[0].fg, OK, "the dot keeps its colour");
        assert_ne!(rows[0].2[2].fg, FG);
        assert_eq!(rows[6].2[2].fg, FG, "the answer is not dimmed");
    }

    #[test]
    fn markdown_bits() {
        let code = Rgb(177, 185, 249);
        let mut call = plain("  Run cargo now.");
        for c in &mut call.2[6..11] {
            c.fg = code;
        }
        let mut rows = vec![
            dot(WHITE, "Intro"),
            plain(""),
            row("  Heading Two", FG, Flags::BOLD),
            plain(""),
            plain("  - first"),
            plain("    - nested"),
            plain(""),
            plain("  ---"),
            plain(""),
            call,
        ];
        let d = analyze(&mut rows, None, &mut HashMap::new());
        assert_eq!(d.headings.len(), 1);
        assert_eq!((d.headings[0].row, d.headings[0].text.as_str(), d.headings[0].level), (2, "Heading Two", 2));
        assert_eq!(rows[4].2[2].c, '•');
        assert_eq!(rows[5].2[4].c, '◦');
        assert_eq!(d.rules, vec![(7, 2)]);
        assert!(d.hide[7]);
        assert_eq!(d.pills, vec![(9, 6, 10)]);
    }

    #[test]
    fn the_input_box_is_left_alone() {
        let line = "─".repeat(60);
        let mut rows = vec![dot(OK, "Bash(ls)"), plain("  ⎿  a"), plain(&line), plain("❯ - typing"), plain(&line), plain("  Model  Opus")];
        let d = analyze(&mut rows, None, &mut HashMap::new());
        assert_eq!(d.rails.len(), 1);
        assert_eq!(rows[3].2[2].c, '-');
        assert_eq!(rows[5].2[2].fg, FG);
    }

    #[test]
    fn the_pinned_question_does_not_hide_the_answer() {
        let mut q = plain("❯ Rendering test, do not edit any files…");
        q.2.iter_mut().for_each(|c| c.bg = Rgb(0x37, 0x37, 0x37));
        let mut rows = vec![q, plain(""), plain("  - First bullet"), plain(""), plain("  ---")];
        let d = analyze(&mut rows, None, &mut HashMap::new());
        assert_eq!(rows[2].2[2].c, '•');
        assert_eq!(d.rules, vec![(4, 2)]);
    }

    #[test]
    fn rows_seen_before_keep_their_kind() {
        let mut seen = HashMap::new();
        let mut rows = vec![dot(OK, "Bash(ls)"), plain("      out 1"), plain("      out 2")];
        analyze(&mut rows, None, &mut seen);
        // Scrolled: the "●" row is gone, the output rows are at the top.
        let mut rows = vec![plain("      out 1"), plain("      out 2")];
        let d = analyze(&mut rows, None, &mut seen);
        assert_eq!(d.rails.len(), 1);
    }
}
