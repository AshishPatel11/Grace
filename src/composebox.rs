//! The typing box under a Claude tab: where it sits, keys, mouse, sending, drawing.
//! The text editing itself (caret, selection, undo) is in `compose.rs`.
//!
//! How keys are split between the box and Claude:
//! - Letters, digits, editing keys (arrows, Backspace, Ctrl+Z ...) edit the box while it has text.
//! - When the box is empty, Enter, arrows, Backspace, Tab, Esc go to Claude as before,
//!   so Claude's own menus and history keep working.
//! - Esc, Tab, Shift+Tab, PgUp/PgDn and Alt+keys always go to Claude.
//! - Typing "/" in an empty box sends it straight to Claude, so Claude's command menu
//!   works; the box takes over again after Enter or Esc.
//! - While Claude asks a question (permission, choice), every key goes to Claude.

use std::time::Duration;

use alacritty_terminal::grid::Scroll;
use windows::core::BOOL;
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS};
use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, SetCapture, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY};

use crate::app::{App, Drag};
use crate::input::*;
use crate::layout::Rect;
use crate::theme::*;
use crate::win;

/// The box grows with the text up to this many lines, then scrolls.
pub const MAX_LINES: usize = 8;
const MARGIN: f32 = 6.0;
const PAD_X: f32 = 10.0;
const PAD_Y: f32 = 7.0;

#[derive(Clone, Copy, Debug)]
pub struct BoxHit {
    pub id: u64,
    /// The whole box.
    pub rect: Rect,
    /// Where the text is drawn (one text row per visible line).
    pub text: Rect,
    pub mic: Rect,
}

/// One line of the laid-out text.
#[derive(Clone, Copy, Debug)]
struct LineInfo {
    start: usize,
    /// Length including the new-line character, if any.
    len: usize,
    nl: usize,
    top: f32,
    h: f32,
}

fn lines_of(l: &IDWriteTextLayout) -> Vec<LineInfo> {
    unsafe {
        let mut n = 0u32;
        let _ = l.GetLineMetrics(None, &mut n);
        let mut m = vec![DWRITE_LINE_METRICS::default(); n.max(1) as usize];
        if l.GetLineMetrics(Some(&mut m), &mut n).is_err() {
            return vec![LineInfo { start: 0, len: 0, nl: 0, top: 0.0, h: 16.0 }];
        }
        let mut out = Vec::with_capacity(n as usize);
        let (mut start, mut top) = (0usize, 0f32);
        for x in m.iter().take(n as usize) {
            out.push(LineInfo { start, len: x.length as usize, nl: x.newlineLength as usize, top, h: x.height });
            start += x.length as usize;
            top += x.height;
        }
        out
    }
}

fn text_height(l: &IDWriteTextLayout) -> f32 {
    let mut m = DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = l.GetMetrics(&mut m);
    }
    m.height
}

/// Which line a caret position is on. A position right after a new line (or at a
/// wrap point) belongs to the next line.
fn line_index(lines: &[LineInfo], pos: usize) -> usize {
    lines.iter().position(|li| pos < li.start + li.len).unwrap_or(lines.len().saturating_sub(1))
}

/// Caret position (x, y) inside the laid-out text.
fn caret_xy(l: &IDWriteTextLayout, lines: &[LineInfo], text: &[u16], pos: usize) -> (f32, f32) {
    let i = line_index(lines, pos);
    let Some(li) = lines.get(i) else { return (0.0, 0.0) };
    if pos <= li.start || pos == 0 {
        return (0.0, li.top);
    }
    let prev = pos - 1;
    let (mut x, mut y) = (0f32, 0f32);
    let mut m = DWRITE_HIT_TEST_METRICS::default();
    unsafe {
        let _ = l.HitTestTextPosition(prev as u32, true, &mut x, &mut y, &mut m);
    }
    if text.get(prev) == Some(&(b'\n' as u16)) {
        x = 0.0;
    }
    (x, li.top)
}

/// Text position under a point (relative to the text's top-left).
fn hit(l: &IDWriteTextLayout, text: &[u16], x: f32, y: f32) -> usize {
    let mut trailing = BOOL(0);
    let mut inside = BOOL(0);
    let mut m = DWRITE_HIT_TEST_METRICS::default();
    unsafe {
        if l.HitTestPoint(x, y, &mut trailing, &mut inside, &mut m).is_err() {
            return text.len();
        }
    }
    let p = m.textPosition as usize;
    // Clicking right of a line's end hits its new-line character: stay on that line.
    if text.get(p) == Some(&(b'\n' as u16)) {
        return p;
    }
    (if trailing.as_bool() { p + m.length as usize } else { p }).min(text.len())
}

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

impl App {
    /// Should this pane show a typing box?
    pub fn box_wanted(&self, id: u64) -> bool {
        self.panes.get(&id).is_some_and(|p| p.dormant.is_none() && (self.cfg.input_box != p.compose.toggled) && p.is_claude())
    }

    fn box_hit(&self, id: u64) -> Option<BoxHit> {
        self.ui.boxes.iter().find(|b| b.id == id).copied()
    }

    /// The focused pane, if keys typed now should go into its box.
    pub fn box_takes_keys(&self) -> Option<u64> {
        let id = self.focused_id()?;
        let p = self.panes.get(&id)?;
        if self.box_hit(id).is_none() || p.compose.passthrough || p.claude_status() == Some("asking") {
            return None;
        }
        Some(id)
    }

    /// The box text laid out for `width` (re-measured only when the text changes).
    fn box_layout(&mut self, id: u64, width: f32) -> Option<IDWriteTextLayout> {
        let p = self.panes.get_mut(&id)?;
        let w = width.round() as u32;
        if let Some((v, lw, l)) = &p.compose_layout {
            if *v == p.compose.version && *lw == w {
                return Some(l.clone());
            }
        }
        let l = self.gfx.wrapped(&p.compose.text, width)?;
        p.compose_layout = Some((p.compose.version, w, l.clone()));
        Some(l)
    }

    /// Where the box goes inside pane rect `r`, and how many text lines it shows.
    pub fn box_geometry(&mut self, id: u64, r: Rect) -> (BoxHit, usize) {
        let (m, px, py) = (self.s(MARGIN), self.s(PAD_X), self.s(PAD_Y));
        let ch = self.gfx.cell_h;
        let mic = (ch + self.s(8.0)).round();
        let tw = (r.w - 2.0 * m - 2.0 * px - mic - self.s(4.0)).max(40.0).floor();
        let lines = self.box_layout(id, tw).map(|l| lines_of(&l).len()).unwrap_or(1).clamp(1, MAX_LINES);
        let h = (lines as f32 * ch + 2.0 * py).round();
        let rect = Rect::new((r.x + m).round(), (r.bottom() - h - m).round(), (r.w - 2.0 * m).round(), h);
        let text = Rect::new(rect.x + px, rect.y + py, tw, lines as f32 * ch);
        let micr = Rect::new(rect.right() - mic - self.s(4.0), (text.bottom() - ch / 2.0 - mic / 2.0).round(), mic, mic);
        (BoxHit { id, rect, text, mic: micr }, lines)
    }

    /// After the text changed: grow or shrink the box if the line count changed.
    fn box_changed(&mut self, id: u64) {
        if let Some(b) = self.box_hit(id) {
            let lines = self.box_layout(id, b.text.w).map(|l| lines_of(&l).len()).unwrap_or(1).clamp(1, MAX_LINES);
            if self.panes.get(&id).is_some_and(|p| p.compose.shown_lines != lines) {
                self.relayout();
            }
        }
        self.cursor_on = true;
        self.invalidate();
    }

    /// Re-check which panes need a box (Claude started or ended, a question appeared).
    pub fn box_check(&mut self) {
        let ids: Vec<u64> = self.panes.keys().copied().collect();
        if ids.iter().any(|id| self.box_wanted(*id) != self.panes[id].compose.shown || self.toolbar_wanted(*id) != self.panes[id].compose.bar) {
            self.relayout();
            self.invalidate();
        }
    }

    pub fn box_toggle(&mut self) {
        let Some(id) = self.focused_id() else { return };
        if let Some(p) = self.panes.get_mut(&id) {
            if !p.is_claude() {
                return;
            }
            p.compose.toggled = !p.compose.toggled;
            p.compose.passthrough = false;
        }
        let open = self.box_wanted(id);
        let msg = if open {
            "Rich input open: edit like in Word, Enter sends. Ctrl+Shift+I closes it."
        } else {
            "Rich input closed: you type straight into Claude again."
        };
        self.show_toast(msg.into());
        self.guess_clear();
        self.relayout();
        self.invalidate();
    }

    /// Send the box text to Claude: typed in, then Enter.
    fn box_send(&mut self, id: u64) {
        let Some(p) = self.panes.get_mut(&id) else { return };
        let Some(l) = &p.live else { return };
        let text = p.compose.take();
        {
            let mut t = l.term.lock();
            if t.grid().display_offset() != 0 {
                t.scroll_display(Scroll::Bottom);
            }
            t.selection = None;
        }
        let tx = l.tx.clone();
        std::thread::spawn(move || {
            use alacritty_terminal::event_loop::Msg;
            use std::borrow::Cow;
            let clean = text.replace('\x1b', "");
            // Several lines (or a lot of text) go in as one paste, so the new lines do not
            // send the message early. A short line is typed, so "/" and "!" act as usual.
            let paste = clean.contains('\n') || clean.len() > 300;
            let body = if paste { format!("\x1b[200~{}\x1b[201~", clean.replace('\n', "\r")) } else { clean };
            let _ = tx.send(Msg::Input(Cow::Owned(body.into_bytes())));
            // Claude must see the text before the Enter, or the Enter is taken as part of it.
            std::thread::sleep(Duration::from_millis(if paste { 160 } else { 50 }));
            let _ = tx.send(Msg::Input(Cow::Borrowed(b"\r")));
        });
        self.box_changed(id);
    }

    /// Paste the clipboard into the box. A picture goes to Claude (Alt+V), like before.
    pub fn box_paste(&mut self, id: u64) {
        match win::clipboard_text(self.hwnd) {
            Some(t) if !t.is_empty() => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.compose.insert(&t);
                }
                self.box_changed(id);
            }
            _ => {
                if win::clipboard_has_image() {
                    self.note_pasted_image(id);
                    self.write_focused(b"\x1bv".to_vec());
                }
            }
        }
    }

    /// Open Windows voice typing (the same as pressing Win+H). It types into the box.
    pub fn start_voice(&mut self) {
        let inputs = [key_input(0x5B, false), key_input(0x48, false), key_input(0x48, true), key_input(0x5B, true)];
        unsafe {
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }

    /// Keys for the box. Returns true when the box used the key.
    pub fn box_key(&mut self, vk: u16, ctrl: bool, shift: bool, alt: bool) -> bool {
        // A "/" command typed straight into Claude: Enter or Esc ends it.
        if let Some(id) = self.focused_id() {
            if self.box_hit(id).is_some() {
                let p = self.panes.get_mut(&id).unwrap();
                if p.compose.passthrough {
                    if vk == VK_ESCAPE {
                        // Cancel the command: delete what was typed into Claude's prompt,
                        // or the next message from the box would be glued onto it.
                        // Extra Backspaces cover text that Tab filled in (an empty prompt ignores them).
                        let n = p.compose.passthrough_len.max(1) + 30;
                        p.compose.passthrough = false;
                        p.compose.passthrough_len = 0;
                        self.write_focused(vec![0x7f; n]);
                        self.invalidate();
                        return true;
                    }
                    match vk {
                        VK_RETURN => p.compose.passthrough = false,
                        VK_BACK if ctrl => p.compose.passthrough = false,
                        VK_BACK => {
                            p.compose.passthrough_len = p.compose.passthrough_len.saturating_sub(1);
                            if p.compose.passthrough_len == 0 {
                                p.compose.passthrough = false;
                            }
                        }
                        0x43 if ctrl => p.compose.passthrough = false,
                        _ => {}
                    }
                    if !p.compose.passthrough {
                        self.invalidate();
                    }
                    return false;
                }
            }
        }
        let Some(id) = self.box_takes_keys() else { return false };
        if alt {
            return false;
        }
        let Some(b) = self.box_hit(id) else { return false };
        let empty = self.panes[&id].compose.is_empty();
        let has_sel = self.panes[&id].compose.selection().is_some();
        match vk {
            VK_RETURN if shift || ctrl => self.panes.get_mut(&id).unwrap().compose.insert("\n"),
            VK_RETURN if empty => return false,
            VK_RETURN => {
                self.box_send(id);
                return true;
            }
            VK_BACK if empty => return false,
            VK_BACK => self.panes.get_mut(&id).unwrap().compose.backspace(ctrl),
            VK_DELETE if empty => return false,
            VK_DELETE if shift && !ctrl => {
                // Shift+Delete = cut, like Word.
                if let Some(t) = self.panes.get_mut(&id).unwrap().compose.cut() {
                    win::set_clipboard_text(self.hwnd, &t);
                }
            }
            VK_DELETE => self.panes.get_mut(&id).unwrap().compose.delete(ctrl),
            VK_LEFT | VK_RIGHT if empty => return false,
            VK_LEFT => self.panes.get_mut(&id).unwrap().compose.left(ctrl, shift),
            VK_RIGHT => self.panes.get_mut(&id).unwrap().compose.right(ctrl, shift),
            VK_UP | VK_DOWN if ctrl && !shift => {
                let c = &mut self.panes.get_mut(&id).unwrap().compose;
                if c.history.is_empty() {
                    return false;
                }
                c.history_step(vk == VK_UP);
            }
            VK_UP | VK_DOWN if empty => return false,
            VK_UP | VK_DOWN => self.box_line_move(id, b, vk == VK_UP, shift),
            VK_HOME | VK_END if empty => return false,
            VK_HOME | VK_END if ctrl => {
                let c = &mut self.panes.get_mut(&id).unwrap().compose;
                let to = if vk == VK_HOME { 0 } else { c.text.len() };
                c.move_to(to, shift);
            }
            VK_HOME | VK_END => self.box_home_end(id, b, vk == VK_HOME, shift),
            VK_INSERT if shift && !ctrl => self.box_paste(id),
            VK_INSERT if ctrl && has_sel => {
                if let Some(t) = self.panes[&id].compose.selected_text() {
                    win::set_clipboard_text(self.hwnd, &t);
                }
            }
            0x41 if ctrl && !shift && !empty => self.panes.get_mut(&id).unwrap().compose.select_all(), // A
            0x43 if ctrl && has_sel => {
                // C
                if let Some(t) = self.panes[&id].compose.selected_text() {
                    win::set_clipboard_text(self.hwnd, &t);
                }
            }
            0x58 if ctrl && !shift && has_sel => {
                // X
                if let Some(t) = self.panes.get_mut(&id).unwrap().compose.cut() {
                    win::set_clipboard_text(self.hwnd, &t);
                }
            }
            0x56 if ctrl => self.box_paste(id),                                    // V, Ctrl+Shift+V
            0x5A if ctrl && !shift => self.panes.get_mut(&id).unwrap().compose.undo(), // Z
            0x59 if ctrl && !shift => self.panes.get_mut(&id).unwrap().compose.redo(), // Y
            _ => return false,
        }
        self.box_changed(id);
        true
    }

    /// Up / Down by one visual line, keeping the caret's column.
    fn box_line_move(&mut self, id: u64, b: BoxHit, up: bool, select: bool) {
        let Some(l) = self.box_layout(id, b.text.w) else { return };
        let lines = lines_of(&l);
        let c = &mut self.panes.get_mut(&id).unwrap().compose;
        let (x, _) = caret_xy(&l, &lines, &c.text, c.caret);
        let goal = c.goal_x.unwrap_or(x);
        let i = line_index(&lines, c.caret);
        let to = if up && i == 0 {
            0
        } else if !up && i + 1 >= lines.len() {
            c.text.len()
        } else {
            let li = lines[if up { i - 1 } else { i + 1 }];
            hit(&l, &c.text, goal, li.top + li.h / 2.0)
        };
        c.move_to(to, select);
        c.goal_x = Some(goal);
    }

    /// Home / End of the visual line.
    fn box_home_end(&mut self, id: u64, b: BoxHit, home: bool, select: bool) {
        let Some(l) = self.box_layout(id, b.text.w) else { return };
        let lines = lines_of(&l);
        let c = &mut self.panes.get_mut(&id).unwrap().compose;
        let i = line_index(&lines, c.caret);
        let li = lines[i];
        let to = if home {
            li.start
        } else if li.nl > 0 || i + 1 >= lines.len() {
            li.start + li.len - li.nl
        } else {
            // A wrapped line: stop before the space the line broke at.
            (li.start + li.len).saturating_sub(1).max(li.start)
        };
        c.move_to(to, select);
    }

    /// Typed characters. Returns true when the box used them.
    pub fn box_char(&mut self, s: &str, sys: bool) -> bool {
        if sys || s.chars().any(|c| c.is_control()) {
            return false;
        }
        if let Some(id) = self.focused_id() {
            if self.box_hit(id).is_some() {
                let c = &mut self.panes.get_mut(&id).unwrap().compose;
                if c.passthrough {
                    c.passthrough_len += 1;
                    return false;
                }
            }
        }
        let Some(id) = self.box_takes_keys() else { return false };
        let c = &mut self.panes.get_mut(&id).unwrap().compose;
        if c.is_empty() && s == "/" {
            // Let Claude show its command menu: this command goes straight to Claude.
            c.passthrough = true;
            c.passthrough_len = 1;
            self.invalidate();
            return false;
        }
        c.insert(s);
        self.box_changed(id);
        true
    }

    /// Where the Windows input window (other languages, voice typing) should appear.
    pub fn ime_point(&mut self) -> Option<(f32, f32)> {
        if let Some(id) = self.box_takes_keys() {
            let b = self.box_hit(id)?;
            let l = self.box_layout(id, b.text.w)?;
            let lines = lines_of(&l);
            let c = &self.panes[&id].compose;
            let (x, y) = caret_xy(&l, &lines, &c.text, c.caret);
            return Some((b.text.x + x, b.text.y + y - c.scroll));
        }
        let id = self.focused_id()?;
        let (_, r) = self.ui.panes.iter().find(|(p, _)| *p == id).copied()?;
        let l = self.panes.get(&id)?.live.as_ref()?;
        let c = l.term.lock().grid().cursor.point;
        let (ox, oy) = self.text_origin(r);
        Some((ox + c.column.0 as f32 * self.gfx.cell_w, oy + c.line.0 as f32 * self.gfx.cell_h))
    }

    // ---------------- mouse ----------------

    pub fn box_at(&self, x: f32, y: f32) -> Option<BoxHit> {
        self.ui.boxes.iter().find(|b| b.rect.contains(x, y)).copied()
    }

    fn box_pos(&mut self, b: BoxHit, x: f32, y: f32) -> usize {
        let Some(l) = self.box_layout(b.id, b.text.w) else { return 0 };
        let c = &self.panes[&b.id].compose;
        let ty = (y - b.text.y + c.scroll).max(0.0);
        hit(&l, &c.text, x - b.text.x, ty)
    }

    /// Mouse press on a box. Returns true when it was on a box.
    pub fn box_mouse_down(&mut self, x: f32, y: f32, button: u8, shift: bool, count: u32) -> bool {
        let Some(b) = self.box_at(x, y) else { return false };
        self.focus_pane(b.id);
        if button != 0 {
            return true;
        }
        if b.mic.contains(x, y) {
            self.start_voice();
            return true;
        }
        let pos = self.box_pos(b, x, y);
        let c = &mut self.panes.get_mut(&b.id).unwrap().compose;
        match count {
            2 => {
                let (s, e) = c.word_at(pos);
                c.select(s, e);
            }
            3 => {
                let (s, e) = c.paragraph_at(pos);
                c.select(s, e);
            }
            _ => c.move_to(pos, shift),
        }
        c.goal_x = None;
        self.ui.drag = Drag::Compose(b.id);
        unsafe {
            SetCapture(self.hwnd);
        }
        self.cursor_on = true;
        self.invalidate();
        true
    }

    /// Dragging inside a box selects text.
    pub fn box_drag(&mut self, id: u64, x: f32, y: f32) {
        let Some(b) = self.box_hit(id) else { return };
        let pos = self.box_pos(b, x, y);
        let c = &mut self.panes.get_mut(&id).unwrap().compose;
        if c.anchor.is_none() {
            c.anchor = Some(c.caret);
        }
        c.caret = pos;
        self.invalidate();
    }

    /// Mouse wheel over a box: scroll its text. Returns true when it was on a box.
    pub fn box_wheel(&mut self, x: f32, y: f32, delta: i16) -> bool {
        let Some(b) = self.box_at(x, y) else { return false };
        let Some(l) = self.box_layout(b.id, b.text.w) else { return true };
        let total = text_height(&l);
        let c = &mut self.panes.get_mut(&b.id).unwrap().compose;
        let max = (total - b.text.h).max(0.0);
        c.scroll = (c.scroll - delta as f32 / 120.0 * 3.0 * self.gfx.cell_h).clamp(0.0, max);
        self.invalidate();
        true
    }

    // ---------------- drawing ----------------

    pub fn paint_boxes(&mut self) {
        let boxes = self.ui.boxes.clone();
        let focus = self.focused_id();
        let takes = self.box_takes_keys();
        let (mx, my) = self.ui.mouse;
        for b in boxes {
            let Some(l) = self.box_layout(b.id, b.text.w) else { continue };
            let lines = lines_of(&l);
            let total = text_height(&l);
            let ch = self.gfx.cell_h;
            let p = self.panes.get_mut(&b.id).unwrap();
            let c = &mut p.compose;
            let active = takes == Some(b.id) && self.focused;
            // Keep the caret in view.
            let (cx, cy) = caret_xy(&l, &lines, &c.text, c.caret);
            if cy < c.scroll {
                c.scroll = cy;
            } else if cy + ch > c.scroll + b.text.h {
                c.scroll = cy + ch - b.text.h;
            }
            c.scroll = c.scroll.clamp(0.0, (total - b.text.h).max(0.0));
            let scroll = c.scroll;
            let sel = c.selection();
            let empty = c.is_empty();
            let passthrough = c.passthrough;
            let asking = p.claude_status() == Some("asking");

            let g = &self.gfx;
            let radius = self.s(7.0);
            g.round(b.rect, radius, CARD, 1.0);
            let border = if active { ACCENT.mix(BORDER, 0.45) } else { BORDER };
            g.outline(b.rect, radius, border, 1.0);

            g.clip(Rect::new(b.text.x - 2.0, b.text.y, b.text.w + 4.0, b.text.h));
            let (ox, oy) = (b.text.x, b.text.y - scroll);
            if let Some((s, e)) = sel {
                let mut n = 0u32;
                unsafe {
                    let _ = l.HitTestTextRange(s as u32, (e - s) as u32, ox, oy, None, &mut n);
                    let mut hits = vec![DWRITE_HIT_TEST_METRICS::default(); n as usize];
                    if l.HitTestTextRange(s as u32, (e - s) as u32, ox, oy, Some(&mut hits), &mut n).is_ok() {
                        for h in hits.iter().take(n as usize) {
                            // An empty line inside the selection still shows a small mark.
                            let w = h.width.max(g.cell_w * 0.5);
                            g.fill(Rect::new(h.left, h.top, w, h.height.max(ch)), SELECTION);
                        }
                    }
                }
            }
            if empty {
                let hint = if passthrough {
                    "Typing goes straight to Claude for this / command. Enter or Esc ends it."
                } else if asking {
                    "Claude is asking you something above. Your keys go to Claude."
                } else {
                    "Type here. Enter sends, Shift+Enter adds a line. Win+H or the mic to talk."
                };
                g.text(hint, &g.ui, Rect::new(ox + self.s(2.0), oy, b.text.w, ch), TEXT_DIM);
            } else {
                g.draw_layout(&l, ox, oy, if asking || passthrough { TEXT_DIM } else { FG });
            }
            if active && self.cursor_on {
                let thin = (2.0 * g.scale).round().max(1.0);
                g.fill(Rect::new((ox + cx).round(), (oy + cy).round(), thin, ch), CURSOR);
            }
            g.unclip();

            // Mic button.
            let hot = b.mic.contains(mx, my);
            if hot {
                g.round(b.mic, b.mic.h / 2.0, TAB_HOVER, 1.0);
            }
            g.mic(b.mic.cx(), b.mic.cy(), b.mic.h * 0.52, if hot { TEXT } else { TEXT_DIM });
            let _ = focus;
        }
    }

    /// What the box sees at a point (for the pipe, to test clicks).
    pub fn box_probe(&mut self, x: f32, y: f32) -> serde_json::Value {
        let Some(b) = self.box_at(x, y) else { return serde_json::json!({"ok": true, "on_box": false}) };
        let layout = self.box_layout(b.id, b.text.w).is_some();
        let pos = self.box_pos(b, x, y);
        serde_json::json!({"ok": true, "on_box": true, "layout": layout, "pos": pos, "text_rect": [b.text.x, b.text.y, b.text.w, b.text.h], "scroll": self.panes[&b.id].compose.scroll})
    }

    /// Text and state of the focused box (for the pipe, to test it).
    pub fn box_state(&self) -> serde_json::Value {
        let Some(id) = self.focused_id() else { return serde_json::json!({"ok": false}) };
        let Some(p) = self.panes.get(&id) else { return serde_json::json!({"ok": false}) };
        let c = &p.compose;
        serde_json::json!({
            "ok": true,
            "shown": self.box_hit(id).is_some(),
            "takes_keys": self.box_takes_keys() == Some(id),
            "text": c.string(),
            "caret": c.caret,
            "selection": c.selection(),
            "lines": c.shown_lines,
            "passthrough": c.passthrough,
            "toggled": c.toggled,
            "guesses": p.predict.guesses.len(),
            "guess_cursor": p.predict.cursor,
            "box": self.box_hit(id).map(|b| [b.rect.x, b.rect.y, b.rect.w, b.rect.h]),
            "mic": self.box_hit(id).map(|b| [b.mic.x, b.mic.y, b.mic.w, b.mic.h]),
        })
    }
}
