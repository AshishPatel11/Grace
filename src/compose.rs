//! The typing box under a Claude tab.
//!
//! Why it exists: without it, every key goes to Claude, Claude redraws its whole
//! screen, and only then do you see the letter. On a busy PC that round trip is slow.
//! Here the text is edited locally (instant, like a normal text box), and only goes
//! to Claude when you press Enter.
//!
//! This file is only the text editing: the text, the caret, the selection, undo.
//! Drawing and line positions (DirectWrite) live in `render.rs` and `app.rs`.
//!
//! Text is kept as UTF-16, because DirectWrite counts positions in UTF-16 units,
//! so a mouse click position can be used as-is.

#[derive(Default)]
pub struct Compose {
    pub text: Vec<u16>,
    /// Caret position (UTF-16 index).
    pub caret: usize,
    /// The other end of the selection, if any text is selected.
    pub anchor: Option<usize>,
    undo: Vec<(Vec<u16>, usize)>,
    redo: Vec<(Vec<u16>, usize)>,
    /// What the last edit was, so a run of typed letters is undone in one step.
    last: Edit,
    /// Goes up on every text change; the drawing code uses it to know when to re-measure.
    pub version: u64,
    /// Pixels scrolled inside the box (when the text is taller than the box).
    pub scroll: f32,
    /// Keep the caret's x when moving up/down through short lines.
    pub goal_x: Option<f32>,
    /// Ctrl+Shift+I or the "Rich input" button flipped the box for this tab
    /// (open when the setting says closed, closed when it says open).
    pub toggled: bool,
    /// Was the toolbar under this tab on screen at the last layout.
    pub bar: bool,
    /// A "/" command is being typed straight into Claude (so its own menu works).
    pub passthrough: bool,
    /// Characters typed in that "/" command (Backspace back to 0 ends it).
    pub passthrough_len: usize,
    /// Lines the box showed at the last layout (a change means the pane must resize).
    pub shown_lines: usize,
    /// Was the box on screen at the last layout.
    pub shown: bool,
    /// Messages sent from this box, oldest first (Ctrl+Up / Ctrl+Down).
    pub history: Vec<Vec<u16>>,
    pub hist_pos: Option<usize>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Edit {
    #[default]
    None,
    Type,
    Delete,
}

fn is_space(c: u16) -> bool {
    c == b' ' as u16 || c == b'\t' as u16 || c == b'\n' as u16
}

/// 0 = space, 1 = letter/digit (any non-ASCII counts as a letter), 2 = punctuation.
fn class(c: u16) -> u8 {
    if is_space(c) {
        0
    } else if c >= 0x80 || (c as u8).is_ascii_alphanumeric() || c == b'_' as u16 {
        1
    } else {
        2
    }
}

fn is_low(c: u16) -> bool {
    (0xDC00..0xE000).contains(&c)
}

impl Compose {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn string(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }

    /// Selected range (start, end), if the selection is not empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.caret {
            return None;
        }
        Some((a.min(self.caret), a.max(self.caret)))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(s, e)| String::from_utf16_lossy(&self.text[s..e]))
    }

    fn changed(&mut self) {
        self.version += 1;
        self.goal_x = None;
        self.hist_pos = None;
    }

    fn save(&mut self, kind: Edit) {
        // A run of typing (or of deleting) is one undo step; anything else starts a new one.
        if kind == Edit::None || kind != self.last {
            self.undo.push((self.text.clone(), self.caret));
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = kind;
    }

    /// Stop grouping: the next edit gets its own undo step (called after caret moves).
    pub fn break_undo(&mut self) {
        self.last = Edit::None;
    }

    fn delete_selection(&mut self) -> bool {
        if let Some((s, e)) = self.selection() {
            self.text.drain(s..e);
            self.caret = s;
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    /// Type or paste text at the caret (replaces the selection).
    pub fn insert(&mut self, s: &str) {
        let clean = s.replace("\r\n", "\n").replace('\r', "\n");
        let u: Vec<u16> = clean.encode_utf16().collect();
        if u.is_empty() {
            return;
        }
        // One letter = part of a typing run; a paste or a space ends the run.
        let kind = if u.len() == 1 && !is_space(u[0]) { Edit::Type } else { Edit::None };
        self.save(if self.selection().is_some() { Edit::None } else { kind });
        self.delete_selection();
        let at = self.caret.min(self.text.len());
        self.text.splice(at..at, u.iter().copied());
        self.caret = at + u.len();
        if kind == Edit::None {
            self.last = Edit::None;
        }
        self.changed();
    }

    fn prev(&self, pos: usize) -> usize {
        if pos == 0 {
            return 0;
        }
        let mut p = pos - 1;
        if p > 0 && is_low(self.text[p]) {
            p -= 1;
        }
        p
    }

    fn next(&self, pos: usize) -> usize {
        let n = self.text.len();
        if pos >= n {
            return n;
        }
        let mut p = pos + 1;
        if p < n && is_low(self.text[p]) {
            p += 1;
        }
        p
    }

    /// Start of the word left of `pos` (Ctrl+Left, Ctrl+Backspace).
    fn word_left(&self, pos: usize) -> usize {
        let mut p = pos;
        while p > 0 && is_space(self.text[p - 1]) {
            p -= 1;
        }
        if p == 0 {
            return 0;
        }
        let k = class(self.text[p - 1]);
        while p > 0 && class(self.text[p - 1]) == k {
            p -= 1;
        }
        p
    }

    /// Start of the next word (Ctrl+Right), like Word: skip this word, then the spaces.
    fn word_right(&self, pos: usize) -> usize {
        let n = self.text.len();
        let mut p = pos;
        if p < n && !is_space(self.text[p]) {
            let k = class(self.text[p]);
            while p < n && class(self.text[p]) == k {
                p += 1;
            }
        }
        while p < n && is_space(self.text[p]) && self.text[p] != b'\n' as u16 {
            p += 1;
        }
        if p == pos && p < n {
            p += 1;
        }
        p
    }

    /// The word around `pos` (double-click).
    pub fn word_at(&self, pos: usize) -> (usize, usize) {
        let n = self.text.len();
        if n == 0 {
            return (0, 0);
        }
        let i = pos.min(n - 1);
        let k = class(self.text[i]);
        let (mut s, mut e) = (i, i);
        while s > 0 && class(self.text[s - 1]) == k {
            s -= 1;
        }
        while e < n && class(self.text[e]) == k {
            e += 1;
        }
        (s, e)
    }

    /// Start and end of the paragraph (text between new lines) around `pos` (triple-click).
    pub fn paragraph_at(&self, pos: usize) -> (usize, usize) {
        let nl = b'\n' as u16;
        let mut s = pos.min(self.text.len());
        while s > 0 && self.text[s - 1] != nl {
            s -= 1;
        }
        let mut e = pos.min(self.text.len());
        while e < self.text.len() && self.text[e] != nl {
            e += 1;
        }
        (s, e)
    }

    /// Move the caret. `select` keeps (or starts) a selection, like holding Shift.
    pub fn move_to(&mut self, pos: usize, select: bool) {
        let pos = pos.min(self.text.len());
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
        } else {
            self.anchor = None;
        }
        self.caret = pos;
        self.break_undo();
    }

    pub fn select(&mut self, start: usize, end: usize) {
        self.anchor = Some(start.min(self.text.len()));
        self.caret = end.min(self.text.len());
        self.break_undo();
    }

    pub fn left(&mut self, word: bool, select: bool) {
        // Left with a selection and no Shift: go to the selection's start (like Word).
        if !select {
            if let Some((s, _)) = self.selection() {
                self.move_to(s, false);
                return;
            }
        }
        let to = if word { self.word_left(self.caret) } else { self.prev(self.caret) };
        self.move_to(to, select);
        self.goal_x = None;
    }

    pub fn right(&mut self, word: bool, select: bool) {
        if !select {
            if let Some((_, e)) = self.selection() {
                self.move_to(e, false);
                return;
            }
        }
        let to = if word { self.word_right(self.caret) } else { self.next(self.caret) };
        self.move_to(to, select);
        self.goal_x = None;
    }

    pub fn backspace(&mut self, word: bool) {
        if self.selection().is_some() {
            self.save(Edit::None);
            self.delete_selection();
            self.changed();
            return;
        }
        if self.caret == 0 {
            return;
        }
        let from = if word { self.word_left(self.caret) } else { self.prev(self.caret) };
        self.save(Edit::Delete);
        self.text.drain(from..self.caret);
        self.caret = from;
        self.anchor = None;
        self.changed();
    }

    pub fn delete(&mut self, word: bool) {
        if self.selection().is_some() {
            self.save(Edit::None);
            self.delete_selection();
            self.changed();
            return;
        }
        if self.caret >= self.text.len() {
            return;
        }
        let to = if word { self.word_right(self.caret) } else { self.next(self.caret) };
        self.save(Edit::Delete);
        self.text.drain(self.caret..to);
        self.anchor = None;
        self.changed();
    }

    pub fn select_all(&mut self) {
        self.select(0, self.text.len());
    }

    /// Cut: returns the removed text.
    pub fn cut(&mut self) -> Option<String> {
        let t = self.selected_text()?;
        self.save(Edit::None);
        self.delete_selection();
        self.changed();
        Some(t)
    }

    pub fn undo(&mut self) {
        if let Some((t, c)) = self.undo.pop() {
            self.redo.push((std::mem::replace(&mut self.text, t), self.caret));
            self.caret = c.min(self.text.len());
            self.anchor = None;
            self.last = Edit::None;
            self.changed();
        }
    }

    pub fn redo(&mut self) {
        if let Some((t, c)) = self.redo.pop() {
            self.undo.push((std::mem::replace(&mut self.text, t), self.caret));
            self.caret = c.min(self.text.len());
            self.anchor = None;
            self.last = Edit::None;
            self.changed();
        }
    }

    /// Replace everything (used by history). Can be undone.
    pub fn set(&mut self, text: Vec<u16>) {
        self.save(Edit::None);
        self.text = text;
        self.caret = self.text.len();
        self.anchor = None;
        self.version += 1;
        self.goal_x = None;
    }

    /// Take the text to send it. Empties the box and remembers the message.
    pub fn take(&mut self) -> String {
        let t = std::mem::take(&mut self.text);
        let s = String::from_utf16_lossy(&t);
        if !s.trim().is_empty() && self.history.last() != Some(&t) {
            self.history.push(t);
            if self.history.len() > 100 {
                self.history.remove(0);
            }
        }
        self.caret = 0;
        self.anchor = None;
        self.undo.clear();
        self.redo.clear();
        self.last = Edit::None;
        self.scroll = 0.0;
        self.changed();
        s
    }

    /// Ctrl+Up / Ctrl+Down: step through messages sent from this box.
    pub fn history_step(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let n = self.history.len();
        let pos = match (self.hist_pos, older) {
            (None, true) => n - 1,
            (None, false) => return,
            (Some(0), true) => 0,
            (Some(p), true) => p - 1,
            (Some(p), false) if p + 1 >= n => {
                self.set(Vec::new());
                self.hist_pos = None;
                return;
            }
            (Some(p), false) => p + 1,
        };
        self.set(self.history[pos].clone());
        self.hist_pos = Some(pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Compose {
        let mut x = Compose::default();
        x.insert(s);
        x
    }

    #[test]
    fn typing_and_undo_groups_a_word() {
        let mut x = Compose::default();
        for ch in "hello".chars() {
            x.insert(&ch.to_string());
        }
        x.insert(" ");
        for ch in "world".chars() {
            x.insert(&ch.to_string());
        }
        assert_eq!(x.string(), "hello world");
        x.undo();
        assert_eq!(x.string(), "hello ");
        x.undo();
        assert_eq!(x.string(), "hello");
        x.redo();
        assert_eq!(x.string(), "hello ");
    }

    #[test]
    fn word_moves_like_word() {
        let mut x = c("git commit -m fix");
        x.move_to(0, false);
        x.right(true, false);
        assert_eq!(x.caret, 4); // start of "commit"
        x.right(true, false);
        assert_eq!(x.caret, 11); // start of "-"
        x.left(true, false);
        assert_eq!(x.caret, 4);
        x.move_to(x.text.len(), false);
        x.backspace(true);
        assert_eq!(x.string(), "git commit -m ");
    }

    #[test]
    fn selection_replace_and_cut() {
        let mut x = c("abc def");
        x.select(0, 3);
        x.insert("X");
        assert_eq!(x.string(), "X def");
        x.select_all();
        assert_eq!(x.cut().as_deref(), Some("X def"));
        assert!(x.is_empty());
        x.undo();
        assert_eq!(x.string(), "X def");
    }

    #[test]
    fn shift_select_then_left_collapses() {
        let mut x = c("abcd");
        x.move_to(1, false);
        x.right(false, true);
        x.right(false, true);
        assert_eq!(x.selection(), Some((1, 3)));
        x.left(false, false);
        assert_eq!((x.caret, x.selection()), (1, None));
    }

    #[test]
    fn emoji_is_one_step() {
        let mut x = c("a😀b");
        x.move_to(x.text.len(), false);
        x.left(false, false);
        x.left(false, false);
        assert_eq!(x.caret, 1);
        x.delete(false);
        assert_eq!(x.string(), "ab");
    }

    #[test]
    fn newlines_are_normalised_and_history_works() {
        let mut x = c("one\r\ntwo");
        assert_eq!(x.string(), "one\ntwo");
        assert_eq!(x.take(), "one\ntwo");
        x.insert("three");
        x.take();
        x.history_step(true);
        assert_eq!(x.string(), "three");
        x.history_step(true);
        assert_eq!(x.string(), "one\ntwo");
        x.history_step(false);
        assert_eq!(x.string(), "three");
        x.history_step(false);
        assert!(x.is_empty());
    }

    #[test]
    fn double_and_triple_click_ranges() {
        let x = c("say hello.world\nnext line");
        assert_eq!(x.word_at(5), (4, 9));
        assert_eq!(x.paragraph_at(2), (0, 15));
        assert_eq!(x.paragraph_at(17), (16, 25));
    }
}
