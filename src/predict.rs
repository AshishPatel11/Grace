//! Letters typed into Claude's own input line show at once.
//!
//! Why: in a long chat Claude sometimes stalls before it draws a typed letter
//! (measured: usually 30 ms, but up to 671 ms in a 93 MB chat). That stall is the lag.
//! So we draw the letter ourselves right away, at the terminal cursor, and drop our copy
//! as soon as Claude's own drawing shows the same letter in that cell. Claude draws the
//! same letter in the same place, so nothing visibly changes when it catches up.
//!
//! We only guess when it is safe: on Claude's prompt (the "❯" line and the lines under it,
//! between the two rules), typing at the end of the text, plain one-cell letters.
//! Anything else (Enter, arrows, Tab, Ctrl keys, paste, a click) drops every guess and
//! shows Claude's real screen. A guess Claude never confirms goes away after 3 s.

use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::app::App;
use crate::gfx::Rgb;
use crate::theme;

/// A guess older than this, still not drawn by Claude, is dropped.
const GIVE_UP: Duration = Duration::from_millis(3000);
/// Do not guess in the last few columns: Claude wraps its text before the edge.
const EDGE: usize = 4;

#[derive(Clone, Copy, Debug)]
pub struct Guess {
    pub line: i32,
    pub col: usize,
    /// The letter we expect (a space for an erased cell).
    pub ch: char,
    /// Backspace erased this cell.
    pub erase: bool,
    /// What the cell showed when we guessed (so a cell that already had this letter
    /// is not taken as Claude's answer).
    pub was: char,
    pub color: Rgb,
    /// First letter in an empty prompt: also hide Claude's grey hint after it.
    pub clear_rest: bool,
    pub at: Instant,
}

/// After a key we could not guess, wait this long with no keys before guessing again,
/// so Claude has drawn that key and the cursor is where it really is.
const RESYNC_QUIET: Duration = Duration::from_millis(250);

#[derive(Default)]
pub struct Predict {
    pub guesses: Vec<Guess>,
    /// Where the cursor is after the guesses.
    pub cursor: Option<(i32, usize)>,
    /// When the last key went to Claude from this pane.
    pub last_key: Option<Instant>,
    /// A key went to Claude that we did not guess: its effect is unknown until Claude
    /// draws it, so no new guesses until things are quiet again.
    pub resync: bool,
}

impl Predict {
    pub fn clear(&mut self) {
        self.guesses.clear();
        self.cursor = None;
        self.resync = true;
    }

    /// May we make a new guess now?
    fn in_sync(&mut self) -> bool {
        if self.resync && self.guesses.is_empty() && self.last_key.map_or(true, |t| t.elapsed() >= RESYNC_QUIET) {
            self.resync = false;
        }
        !self.resync
    }

    /// Drop the guesses Claude has drawn by now. Drop all of them if Claude drew
    /// something else, or took too long.
    pub fn settle<T>(&mut self, t: &Term<T>) {
        if self.guesses.is_empty() {
            return;
        }
        if self.guesses[0].at.elapsed() > GIVE_UP || t.grid().display_offset() != 0 || !t.mode().contains(TermMode::ALT_SCREEN) {
            self.clear();
            return;
        }
        let g = t.grid();
        let lines = t.screen_lines() as i32;
        let cols = t.columns();
        let cur = g.cursor.point;
        let mut done = vec![false; self.guesses.len()];
        for (i, q) in self.guesses.iter().enumerate() {
            if q.line < 0 || q.line >= lines || q.col >= cols || prompt_start(t, q.line).is_none() {
                self.clear();
                return;
            }
            let c = g[Line(q.line)][Column(q.col)].c;
            let passed = cur.line.0 > q.line || (cur.line.0 == q.line && cur.column.0 > q.col);
            done[i] = if q.erase {
                c == ' '
            } else {
                c == q.ch && (q.was != q.ch || passed)
            };
            if !done[i] && c != q.was && c != q.ch && !q.clear_rest {
                // Claude drew something we did not expect: stop guessing.
                self.clear();
                return;
            }
        }
        // Claude handles keys in order: once a later guess shows, the earlier ones are done too.
        if let Some(last) = done.iter().rposition(|d| *d) {
            self.guesses.drain(..=last);
        }
        if self.guesses.is_empty() {
            self.cursor = None;
        }
    }
}

/// If `line` is part of Claude's prompt (between the two rules), the column where the
/// typed text starts. The first prompt line starts with "❯ ", the lines under it are
/// indented, and the line above the "❯" line is a rule ("───").
pub fn prompt_start<T>(t: &Term<T>, line: i32) -> Option<usize> {
    let g = t.grid();
    let lines = t.screen_lines() as i32;
    let first = |l: i32| g[Line(l)][Column(0)].c;
    let mut l = line;
    let mut steps = 0;
    while l >= 0 && steps < 24 {
        match first(l) {
            '\u{276F}' => {
                // The rule above, and a rule somewhere below.
                if l == 0 || first(l - 1) != '\u{2500}' {
                    return None;
                }
                let below = (line + 1..lines.min(line + 24)).any(|b| first(b) == '\u{2500}');
                return below.then_some(2);
            }
            // Lines under the "❯" line are indented.
            ' ' => {}
            _ => return None,
        }
        l -= 1;
        steps += 1;
    }
    None
}

/// A letter we can draw in one cell, the same as Claude will.
fn plain(ch: char) -> bool {
    ch == ' ' || ch.is_ascii_graphic() || (('\u{A1}'..='\u{24F}').contains(&ch) && ch != '\u{AD}')
}

/// What a cell looks like with our guesses on top: (char, is it ours or blanked by us).
fn shown<T>(t: &Term<T>, guesses: &[Guess], line: i32, col: usize) -> (char, bool) {
    if let Some(g) = guesses.iter().rev().find(|g| g.line == line && g.col == col) {
        return (g.ch, true);
    }
    if guesses.iter().any(|g| g.clear_rest && g.line == line && col > g.col) {
        return (' ', true);
    }
    (t.grid()[Line(line)][Column(col)].c, false)
}

/// Is the rest of the row from `col` empty (with our guesses on top), or only Claude's
/// grey hint text? Returns (empty_or_hint, has_hint).
fn rest_with<T>(t: &Term<T>, guesses: &[Guess], line: i32, col: usize) -> (bool, bool) {
    let mut hint = false;
    for c in col..t.columns() {
        let (ch, ours) = shown(t, guesses, line, c);
        if ch == ' ' || ch == '\0' {
            continue;
        }
        if ours {
            return (false, false);
        }
        let cell = &t.grid()[Line(line)][Column(c)];
        let grey = cell.flags.contains(Flags::DIM) || !matches!(cell.fg, Color::Named(NamedColor::Foreground));
        if !grey {
            return (false, false);
        }
        hint = true;
    }
    (true, hint)
}

impl App {
    /// Should keys typed now in pane `id` be guessed? (Claude, its prompt on screen.)
    fn can_guess(&self, id: u64) -> bool {
        self.cfg.instant_typing
            && self.box_takes_keys().is_none()
            && self.panes.get(&id).is_some_and(|p| p.is_claude() && p.live.is_some() && p.claude_status() != Some("asking"))
    }

    /// A key we will not guess goes to Claude: keep the guesses already made (they are
    /// still right), but make no new ones until Claude has caught up.
    fn guess_skip(&mut self, id: u64) {
        if let Some(p) = self.panes.get_mut(&id) {
            p.predict.resync = true;
            p.predict.last_key = Some(Instant::now());
        }
    }

    /// A letter is about to be sent to Claude: draw it now.
    pub fn guess_char(&mut self, s: &str) {
        let Some(id) = self.focused_id() else { return };
        let mut chars = s.chars();
        let (Some(ch), None) = (chars.next(), chars.next()) else {
            return self.guess_skip(id);
        };
        if !plain(ch) || !self.can_guess(id) {
            return self.guess_skip(id);
        }
        let p = self.panes.get_mut(&id).unwrap();
        let sync = p.predict.in_sync();
        p.predict.last_key = Some(Instant::now());
        if !sync {
            return;
        }
        let Some(l) = &p.live else { return };
        let t = l.term.lock();
        let cur = t.grid().cursor.point;
        let (line, col) = p.predict.cursor.unwrap_or((cur.line.0, cur.column.0));
        let guesses = &p.predict.guesses;
        let made = (|| {
            if t.grid().display_offset() != 0 || !t.mode().contains(TermMode::ALT_SCREEN) || line < 0 || line >= t.screen_lines() as i32 {
                return None;
            }
            let start = prompt_start(&*t, line)?;
            if col < start || col + EDGE >= t.columns() {
                return None;
            }
            let first = col == start && guesses.is_empty();
            // These change Claude's mode instead of being typed (help, shell, memory).
            if first && matches!(ch, '?' | '!' | '#' | '&') {
                return None;
            }
            let (empty, hint) = rest_with(&*t, guesses, line, col);
            if !empty || (hint && !first) {
                return None;
            }
            // Same colour as the letter before it (Claude's own text colour).
            let color = match guesses.iter().rev().find(|g| g.line == line && g.col + 1 == col && !g.erase && g.ch != ' ') {
                Some(g) => g.color,
                None if col > start => theme::resolve(t.grid()[Line(line)][Column(col - 1)].fg, t.colors()),
                None => theme::FG,
            };
            // Typed over a cell we blanked with Backspace: compare with what Claude still shows.
            let was = guesses.iter().rev().find(|g| g.line == line && g.col == col && g.erase).map(|g| g.was).unwrap_or(t.grid()[Line(line)][Column(col)].c);
            Some(Guess { line, col, ch, erase: false, was, color, clear_rest: first && hint, at: Instant::now() })
        })();
        drop(t);
        let p = self.panes.get_mut(&id).unwrap();
        match made {
            Some(g) => {
                p.predict.guesses.retain(|x| !(x.line == line && x.col == col && x.erase));
                p.predict.guesses.push(g);
                p.predict.cursor = Some((line, col + 1));
                self.cursor_on = true;
                self.invalidate();
            }
            None => p.predict.resync = true,
        }
    }

    /// Backspace is about to be sent to Claude: take back our last letter, or blank the cell.
    pub fn guess_backspace(&mut self) {
        let Some(id) = self.focused_id() else { return };
        if !self.can_guess(id) {
            return self.guess_skip(id);
        }
        let p = self.panes.get_mut(&id).unwrap();
        let sync = p.predict.in_sync();
        p.predict.last_key = Some(Instant::now());
        if !sync {
            return;
        }
        if let Some(last) = p.predict.guesses.last().copied() {
            if !last.erase {
                p.predict.guesses.pop();
                p.predict.cursor = if p.predict.guesses.is_empty() { None } else { Some((last.line, last.col)) };
                self.invalidate();
                return;
            }
        }
        let Some(l) = &p.live else { return };
        let t = l.term.lock();
        let cur = t.grid().cursor.point;
        let (line, col) = p.predict.cursor.unwrap_or((cur.line.0, cur.column.0));
        let guesses = &p.predict.guesses;
        let made = (|| {
            if t.grid().display_offset() != 0 || line < 0 || line >= t.screen_lines() as i32 {
                return None;
            }
            let start = prompt_start(&*t, line)?;
            if col <= start || col >= t.columns() || rest_with(&*t, guesses, line, col) != (true, false) {
                return None;
            }
            let (was, ours) = shown(&*t, guesses, line, col - 1);
            (was != ' ' && !ours).then(|| Guess { line, col: col - 1, ch: ' ', erase: true, was, color: theme::FG, clear_rest: false, at: Instant::now() })
        })();
        drop(t);
        let p = self.panes.get_mut(&id).unwrap();
        match made {
            Some(g) => {
                p.predict.guesses.push(g);
                p.predict.cursor = Some((line, col - 1));
                self.invalidate();
            }
            None => p.predict.resync = true,
        }
    }

    /// Show Claude's real screen again (Enter, arrows, Ctrl keys, a paste, a click):
    /// these change the prompt in ways we do not guess.
    pub fn guess_clear(&mut self) {
        let focus = self.focused_id();
        let mut any = false;
        for p in self.panes.values_mut() {
            if Some(p.id) == focus {
                p.predict.last_key = Some(Instant::now());
                p.predict.resync = true;
            }
            if !p.predict.guesses.is_empty() {
                p.predict.clear();
                any = true;
            }
        }
        if any {
            self.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_letters() {
        assert!(plain('a') && plain('Z') && plain('/') && plain(' ') && plain('é'));
        assert!(!plain('\r') && !plain('\u{1b}') && !plain('你') && !plain('😀'));
    }
}
