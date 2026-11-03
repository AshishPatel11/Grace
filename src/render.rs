//! Painting the window: tab strip, panes, cards, find box, tooltips.
//!
//! Speed trick: shaping text is the slow part, so each screen row keeps its
//! shaped text (`RowCache`). A row is only re-shaped when its content changes.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::CursorShape;
use windows::Win32::Graphics::DirectWrite::IDWriteTextLayout;

use crate::app::{Alert, App, Drag};
use crate::gfx::{Gfx, Rgb};
use crate::layout::Rect;
use crate::procmon::fmt_bytes;
use crate::theme::{self, *};
use crate::win::wstr;

pub struct Seg {
    col: usize,
    layout: IDWriteTextLayout,
    color: Rgb,
    /// Set for symbol/emoji pieces: drawn from a cached picture instead.
    special: Option<(Vec<u16>, usize, bool, bool)>,
}

#[derive(Default)]
pub struct RowCache {
    segs: Vec<Seg>,
    boxes: Vec<(usize, char, Rgb)>,
}

/// Shaped rows of one pane, keyed by the row's content.
/// Two generations: rows seen this frame and last frame. Older ones are dropped,
/// so the cache never grows past about two screens.
#[derive(Default)]
pub struct RowCaches {
    cur: std::collections::HashMap<u64, RowCache>,
    prev: std::collections::HashMap<u64, RowCache>,
}

impl RowCaches {
    pub fn clear(&mut self) {
        self.cur.clear();
        self.prev.clear();
    }

    fn get_or_build(&mut self, hash: u64, build: impl FnOnce() -> RowCache) -> &RowCache {
        if !self.cur.contains_key(&hash) {
            let v = self.prev.remove(&hash).unwrap_or_else(build);
            self.cur.insert(hash, v);
        }
        &self.cur[&hash]
    }

    fn end_frame(&mut self) {
        self.prev = std::mem::take(&mut self.cur);
    }
}

/// One visible cell, copied out of the terminal with its final colours.
pub struct VCell {
    c: char,
    zw: Option<Box<[char]>>,
    fg: Rgb,
    bg: Rgb,
    flags: Flags,
    sel: bool,
}

struct Snap {
    rows: Vec<(u64, Line, Vec<VCell>)>,
    cols: usize,
    lines: usize,
    off: i32,
    hist: usize,
    cursor: alacritty_terminal::term::RenderableCursor,
    cursor_char: char,
    cursor_wide: bool,
}

fn mix(h: u64, v: u64) -> u64 {
    (h ^ v).wrapping_mul(0x100000001b3)
}

fn cell_colors(cell: &Cell, colors: &alacritty_terminal::term::color::Colors) -> (Rgb, Rgb) {
    let mut fg = theme::resolve(cell.fg, colors);
    let mut bg = theme::resolve(cell.bg, colors);
    if cell.flags.contains(Flags::DIM) {
        fg = fg.mix(bg, 0.36);
    }
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.flags.contains(Flags::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

/// Box-drawing and block characters we draw ourselves, so lines join perfectly
/// between rows (fonts often leave a 1-pixel gap).
fn is_box(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x2503 | 0x250C..=0x254B | 0x2550..=0x2570 | 0x2574..=0x2577 | 0x2580..=0x259F)
}

/// Arms of a line character: (left, right, up, down). 1 light, 2 heavy, 3 double.
fn arms(c: char) -> Option<[u8; 4]> {
    Some(match c as u32 {
        0x2500 => [1, 1, 0, 0],
        0x2501 => [2, 2, 0, 0],
        0x2502 => [0, 0, 1, 1],
        0x2503 => [0, 0, 2, 2],
        0x250C | 0x256D => [0, 1, 0, 1],
        0x2510 | 0x256E => [1, 0, 0, 1],
        0x2514 | 0x2570 => [0, 1, 1, 0],
        0x2518 | 0x256F => [1, 0, 1, 0],
        0x250F => [0, 2, 0, 2],
        0x2513 => [2, 0, 0, 2],
        0x2517 => [0, 2, 2, 0],
        0x251B => [2, 0, 2, 0],
        0x251C => [0, 1, 1, 1],
        0x2524 => [1, 0, 1, 1],
        0x252C => [1, 1, 0, 1],
        0x2534 => [1, 1, 1, 0],
        0x253C => [1, 1, 1, 1],
        0x2523 => [0, 2, 2, 2],
        0x252B => [2, 0, 2, 2],
        0x2533 => [2, 2, 0, 2],
        0x253B => [2, 2, 2, 0],
        0x254B => [2, 2, 2, 2],
        0x2550 => [3, 3, 0, 0],
        0x2551 => [0, 0, 3, 3],
        0x2554 => [0, 3, 0, 3],
        0x2557 => [3, 0, 0, 3],
        0x255A => [0, 3, 3, 0],
        0x255D => [3, 0, 3, 0],
        0x2560 => [0, 3, 3, 3],
        0x2563 => [3, 0, 3, 3],
        0x2566 => [3, 3, 0, 3],
        0x2569 => [3, 3, 3, 0],
        0x256C => [3, 3, 3, 3],
        0x2574 => [1, 0, 0, 0],
        0x2575 => [0, 0, 1, 0],
        0x2576 => [0, 1, 0, 0],
        0x2577 => [0, 0, 0, 1],
        _ => return None,
    })
}

fn draw_box(g: &Gfx, c: char, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
    let th = g.line_th.max(1.0);
    let cx = (x + w / 2.0 - th / 2.0).round();
    let cy = (y + h / 2.0 - th / 2.0).round();
    // Rounded corners ╭ ╮ ╯ ╰: two straight arms joined by a quarter circle.
    if matches!(c as u32, 0x256D..=0x2570) {
        let (mx, my) = (cx + th / 2.0, cy + th / 2.0);
        let r = (w / 2.0).min(h / 2.0).max(2.0);
        let (l, rt, t, b) = (x.round(), (x + w).round(), y.round(), (y + h).round());
        let (pts, cw) = match c as u32 {
            0x256D => ([(mx, b), (mx, my + r), (mx + r, my), (rt, my)], true),
            0x256E => ([(l, my), (mx - r, my), (mx, my + r), (mx, b)], true),
            0x256F => ([(mx, t), (mx, my - r), (mx - r, my), (l, my)], true),
            _ => ([(mx, t), (mx, my - r), (mx + r, my), (rt, my)], false),
        };
        g.corner(pts, r, cw, th, color);
        return;
    }
    if let Some(a) = arms(c) {
        let seg = |weight: u8, horiz: bool, from: f32, to: f32| {
            let t = if weight == 2 { th * 2.0 } else { th };
            if weight == 3 {
                for off in [-th, th] {
                    if horiz {
                        g.fill(Rect::new(from, cy + off, to - from, th), color);
                    } else {
                        g.fill(Rect::new(cx + off, from, th, to - from), color);
                    }
                }
            } else if horiz {
                g.fill(Rect::new(from, cy - (t - th) / 2.0, to - from, t), color);
            } else {
                g.fill(Rect::new(cx - (t - th) / 2.0, from, t, to - from), color);
            }
        };
        let (right, bottom) = ((x + w).round(), (y + h).round());
        let (x, y) = (x.round(), y.round());
        if a[0] > 0 {
            seg(a[0], true, x, cx + th);
        }
        if a[1] > 0 {
            seg(a[1], true, cx, right);
        }
        if a[2] > 0 {
            seg(a[2], false, y, cy + th);
        }
        if a[3] > 0 {
            seg(a[3], false, cy, bottom);
        }
        return;
    }
    // Block shapes (█ ▀ ▌ ▛ ...). Every edge is snapped to a whole pixel, so blocks in
    // next-door cells touch exactly: no thin gap and no blurry seam between them.
    // Claude's logo is made of these.
    let (l, rt, t, b) = (x.round(), (x + w).round(), y.round(), (y + h).round());
    let (mx, my) = ((x + w / 2.0).round(), (y + h / 2.0).round());
    let col = |f: f32| (x + w * f).round();
    let row = |f: f32| (y + h * f).round();
    let fill = |x0: f32, y0: f32, x1: f32, y1: f32| {
        if x1 > x0 && y1 > y0 {
            g.fill(Rect::new(x0, y0, x1 - x0, y1 - y0), color);
        }
    };
    match c as u32 {
        0x2588 => fill(l, t, rt, b),
        0x2580 => fill(l, t, rt, my),
        0x2584 => fill(l, my, rt, b),
        0x258C => fill(l, t, mx, b),
        0x2590 => fill(mx, t, rt, b),
        // ▁ ... ▇: lower 1/8 to 7/8.
        0x2581..=0x2587 => fill(l, row(1.0 - (c as u32 - 0x2580) as f32 / 8.0), rt, b),
        // ▉ ... ▏: left 7/8 to 1/8 (▌ = 4/8 is handled above).
        0x2589..=0x258F => fill(l, t, col((0x2590 - c as u32) as f32 / 8.0), b),
        0x2591..=0x2593 => {
            let a = [0.25, 0.5, 0.75][(c as u32 - 0x2591) as usize];
            g.fill_a(Rect::new(l, t, rt - l, b - t), color, a);
        }
        0x2594 => fill(l, t, rt, row(1.0 / 8.0)),
        0x2595 => fill(col(7.0 / 8.0), t, rt, b),
        // Quadrants ▖ ▗ ▘ ▙ ▚ ▛ ▜ ▝ ▞ ▟: which of the 4 corners are filled.
        0x2596..=0x259F => {
            // Bits: upper-left, upper-right, lower-left, lower-right.
            let q: [bool; 4] = match c as u32 {
                0x2596 => [false, false, true, false],
                0x2597 => [false, false, false, true],
                0x2598 => [true, false, false, false],
                0x2599 => [true, false, true, true],
                0x259A => [true, false, false, true],
                0x259B => [true, true, true, false],
                0x259C => [true, true, false, true],
                0x259D => [false, true, false, false],
                0x259E => [false, true, true, false],
                _ => [false, true, true, true],
            };
            if q[0] {
                fill(l, t, mx, my);
            }
            if q[1] {
                fill(mx, t, rt, my);
            }
            if q[2] {
                fill(l, my, mx, b);
            }
            if q[3] {
                fill(mx, my, rt, b);
            }
        }
        _ => {}
    }
}

impl App {
    pub fn paint(&mut self) {
        if self.gfx.ensure_target(self.hwnd).is_err() {
            return;
        }
        // Time our drawing work only (not the wait for the screen refresh).
        let t0 = std::time::Instant::now();
        self.layout_bar();
        self.gfx.begin(BAR);
        let (w, h) = self.client();
        let top = self.bar_h();
        self.gfx.fill(Rect::new(0.0, top, w, h - top), BG);

        self.ui.jump.clear();
        self.ui.cards.clear();
        let panes = self.ui.panes.clone();
        let multi = panes.len() > 1;
        let focus = self.focused_id();
        for (id, r) in &panes {
            self.gfx.clip(*r);
            self.paint_pane(*id, *r, Some(*id) == focus);
            if multi && Some(*id) != focus {
                self.gfx.fill_a(*r, BG, 0.28);
            }
            self.gfx.unclip();
        }
        self.paint_boxes();
        for d in &self.ui.dividers {
            let hot = matches!(&self.ui.drag, Drag::Divider(x) if x.path == d.path) || d.rect.inset(-self.s(3.0)).contains(self.ui.mouse.0, self.ui.mouse.1);
            let c = if hot { ACCENT } else { BORDER };
            if d.vertical {
                self.gfx.fill(Rect::new(d.rect.cx().round() - 0.5, d.rect.y, 1.0, d.rect.h), c);
            } else {
                self.gfx.fill(Rect::new(d.rect.x, d.rect.cy().round() - 0.5, d.rect.w, 1.0), c);
            }
        }
        self.paint_search();
        self.paint_bar();
        self.paint_tooltip();
        self.paint_toast();
        self.frame_us.push_back(t0.elapsed().as_micros() as u64);
        if self.frame_us.len() > 120 {
            self.frame_us.pop_front();
        }
        if !self.gfx.end() {
            self.gfx.drop_target();
            self.invalidate();
        }
    }

    fn paint_pane(&mut self, id: u64, r: Rect, focused: bool) {
        let (ox, oy) = self.text_origin(r);
        let Some(p) = self.panes.get(&id) else { return };
        if let Some(d) = &p.dormant {
            let d = d.clone();
            let cwd = p.cwd.clone().unwrap_or_default();
            let title = p.display_title();
            self.paint_card(id, r, &d, &cwd, &title);
            return;
        }
        let Some(live) = &p.live else { return };
        let term = live.term.clone();
        let pcols = p.cols.max(1);

        // 1) Copy what is on screen, then let go of the terminal at once, so the
        //    program can keep printing while we draw. Only visible rows are copied:
        //    this is the "virtual scroll" - history length does not change the cost.
        let t_wait = std::time::Instant::now();
        let snap = {
            let t = term.lock();
            let waited = t_wait.elapsed().as_micros() as u64;
            let t_copy = std::time::Instant::now();
            let cols = t.columns().min(pcols);
            let lines = t.screen_lines();
            let grid = t.grid();
            let off = grid.display_offset() as i32;
            let colors = t.colors();
            let sel = t.selection.as_ref().and_then(|s| s.to_range(&*t));
            let mut rows = Vec::with_capacity(lines);
            for row in 0..lines {
                let line = Line(row as i32 - off);
                let gr = &grid[line];
                let mut hash: u64 = 0xcbf29ce484222325;
                let mut cells = Vec::with_capacity(cols);
                for col in 0..cols {
                    let cell = &gr[Column(col)];
                    let (fg, bg) = cell_colors(cell, colors);
                    hash = mix(hash, cell.c as u64);
                    hash = mix(hash, ((fg.0 as u64) << 16) | ((fg.1 as u64) << 8) | fg.2 as u64);
                    hash = mix(hash, cell.flags.bits() as u64);
                    let zw = cell.zerowidth().map(|z| {
                        for c in z {
                            hash = mix(hash, *c as u64);
                        }
                        z.to_vec().into_boxed_slice()
                    });
                    let selected = sel.as_ref().is_some_and(|s| s.contains(Point::new(line, Column(col))));
                    cells.push(VCell { c: cell.c, zw, fg, bg, flags: cell.flags, sel: selected });
                }
                rows.push((hash, line, cells));
            }
            let cursor = t.renderable_content().cursor;
            let cc = &grid[cursor.point.line][cursor.point.column];
            self.perf.0 += waited;
            self.perf.1 += t_copy.elapsed().as_micros() as u64;
            Snap {
                rows,
                cols,
                lines,
                off,
                hist: grid.history_size(),
                cursor,
                cursor_char: cc.c,
                cursor_wide: cc.flags.contains(Flags::WIDE_CHAR),
            }
        };
        let link = self.ui.link.as_ref().filter(|(l, _)| *l == id).map(|(_, l)| l.clone());

        // 2) Draw from the copy.
        let t_draw = std::time::Instant::now();
        let mut shaped = 0u64;
        let g = &self.gfx;
        let (cw, ch) = (g.cell_w, g.cell_h);
        let cols = snap.cols;
        let fast = self.cfg.fast_symbols;
        let p = self.panes.get_mut(&id).unwrap();
        for (row, (hash, line, cells)) in snap.rows.iter().enumerate() {
            let y = oy + row as f32 * ch;
            let mut run: Option<(usize, Rgb)> = None;
            for col in 0..=cols {
                let bg = cells.get(col).and_then(|c| {
                    if c.sel {
                        Some(SELECTION)
                    } else if c.bg != BG {
                        Some(c.bg)
                    } else {
                        None
                    }
                });
                match (run, bg) {
                    (Some((_, c)), Some(b)) if c == b => {}
                    (prev, now) => {
                        if let Some((s, c)) = prev {
                            g.fill(Rect::new(ox + s as f32 * cw, y, (col - s) as f32 * cw, ch), c);
                        }
                        run = now.map(|b| (col, b));
                    }
                }
            }
            // The cache is keyed by the row's content, so a line that scrolled up
            // one row is reused instead of being shaped again.
            let rc = p.cache.get_or_build(*hash, || {
                shaped += 1;
                build_row(g, cells)
            });
            for s in &rc.segs {
                match s.special.as_ref().filter(|_| fast) {
                    Some((t, f, u, k)) => g.draw_special(t, *f, *u, *k, ox + s.col as f32 * cw, y, s.color),
                    None => g.draw_layout(&s.layout, ox + s.col as f32 * cw, y, s.color),
                }
            }
            for (col, c, color) in &rc.boxes {
                draw_box(g, *c, ox + *col as f32 * cw, y, cw, ch, *color);
            }
            if let Some(l) = &link {
                if *line >= l.start.line && *line <= l.end.line {
                    let c0 = if *line == l.start.line { l.start.column.0 } else { 0 };
                    let c1 = if *line == l.end.line { l.end.column.0 } else { cols - 1 };
                    g.fill(Rect::new(ox + c0 as f32 * cw, y + g.underline_y, (c1 + 1 - c0) as f32 * cw, g.line_th), BLUE);
                }
            }
        }
        p.cache.end_frame();
        self.perf.2 += t_draw.elapsed().as_micros() as u64;
        self.perf.3 += shaped;

        // Cursor. Not shown while you type in the box below (the box has its own caret).
        let (cursor, off, lines) = (snap.cursor, snap.off, snap.lines);
        let crow = cursor.point.line.0 + off;
        let boxed = self.box_takes_keys() == Some(id);
        if !boxed && cursor.shape != CursorShape::Hidden && crow >= 0 && (crow as usize) < lines && (self.cursor_on || !focused) {
            let x = ox + cursor.point.column.0 as f32 * cw;
            let y = oy + crow as f32 * ch;
            let wide = if snap.cursor_wide { 2.0 } else { 1.0 };
            let cr = Rect::new(x, y, cw * wide, ch);
            let thin = (2.0 * g.scale).round().max(1.0);
            match (cursor.shape, focused && self.focused) {
                (CursorShape::Block, true) => {
                    g.fill(cr, CURSOR);
                    let c = snap.cursor_char;
                    if c != ' ' && !is_box(c) {
                        let s = wstr(&c.to_string());
                        if let Some(l) = g.make_layout(&s, 0, false, false) {
                            g.draw_layout(&l, x, y, BG);
                        }
                    }
                }
                (CursorShape::Beam, true) => g.fill(Rect::new(x, y, thin, ch), CURSOR),
                (CursorShape::Underline, true) => g.fill(Rect::new(x, y + ch - thin, cw * wide, thin), CURSOR),
                _ => g.outline(cr, 0.0, CURSOR.mix(BG, 0.3), 1.0),
            }
        }

        // Scroll position + "jump to bottom".
        let hist = snap.hist;
        let hover = r.contains(self.ui.mouse.0, self.ui.mouse.1) && self.ui.mouse.0 > r.right() - self.s(14.0);
        if hist > 0 && (off > 0 || hover || matches!(self.ui.drag, Drag::Scroll(x) if x == id)) {
            let total = (hist + lines) as f32;
            let th = (r.h * lines as f32 / total).max(self.s(24.0));
            let frac = (hist as f32 - off as f32) / hist.max(1) as f32;
            let ty = r.y + (r.h - th) * frac;
            let bw = self.s(if hover { 8.0 } else { 5.0 });
            g.round(Rect::new(r.right() - bw - self.s(3.0), ty, bw, th), bw / 2.0, TEXT_DIM, 0.55);
        }
        if off > 0 {
            let label = format!("↓  {off} lines below");
            let w = g.measure(&label, &g.ui_small) + self.s(24.0);
            let cr = Rect::new(r.right() - w - self.s(18.0), r.bottom() - self.s(38.0), w, self.s(26.0));
            g.round(cr, cr.h / 2.0, CARD, 0.96);
            g.outline(cr, cr.h / 2.0, BORDER, 1.0);
            g.text(&label, &g.ui_center, cr, TEXT);
            self.ui.jump.push((id, cr));
        }
    }

    fn paint_card(&mut self, id: u64, r: Rect, d: &crate::pane::Dormant, cwd: &str, title: &str) {
        let g = &self.gfx;
        let w = self.s(420.0).min(r.w - self.s(40.0));
        let h = self.s(150.0);
        let cr = Rect::new(r.cx() - w / 2.0, r.cy() - h / 2.0, w, h);
        g.round(cr, self.s(12.0), CARD, 1.0);
        g.outline(cr, self.s(12.0), BORDER, 1.0);
        let pad = self.s(20.0);
        let line = |i: f32, s: &str, f: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat, c: Rgb| {
            g.text(s, f, Rect::new(cr.x + pad, cr.y + pad + i * self.s(24.0), cr.w - 2.0 * pad, self.s(22.0)), c);
        };
        let head = if d.claude_session.is_some() { format!("Sleeping chat: {title}") } else { format!("Saved tab: {title}") };
        line(0.0, &head, &g.ui_bold, TEXT);
        line(1.0, &format!("Folder: {}", if cwd.is_empty() { "-" } else { cwd }), &g.ui, TEXT_DIM);
        let note = match d.freed_mb {
            Some(mb) => format!("Asleep. This freed {} of RAM.", fmt_bytes(mb * 1_048_576)),
            None => "Not started yet, so it uses no RAM.".to_string(),
        };
        line(2.0, &note, &g.ui, TEXT_DIM);
        line(3.6, "Press Enter or click here to resume", &g.ui_bold, ACCENT);
        self.ui.cards.push((id, cr));
    }

    fn paint_search(&self) {
        let Some(s) = &self.ui.search else { return };
        let Some((_, r)) = self.ui.panes.iter().find(|(id, _)| *id == s.pane) else { return };
        let g = &self.gfx;
        let w = self.s(330.0).min(r.w - self.s(20.0));
        let br = Rect::new(r.right() - w - self.s(14.0), r.y + self.s(8.0), w, self.s(32.0));
        g.round(br, self.s(8.0), CARD, 0.98);
        g.outline(br, self.s(8.0), if s.found == Some(false) { RED } else { ACCENT }, 1.0);
        let q = format!("Find: {}▏", s.query);
        g.text(&q, &g.ui, Rect::new(br.x + self.s(12.0), br.y, br.w - self.s(140.0), br.h), TEXT);
        let hint = match s.found {
            Some(false) => "no match",
            _ => "Enter older · Shift+Enter newer",
        };
        g.text(hint, &g.ui_small, Rect::new(br.right() - self.s(132.0), br.y, self.s(126.0), br.h), TEXT_DIM);
    }

    fn paint_bar(&self) {
        let g = &self.gfx;
        let (mx, my) = self.ui.mouse;
        let s = g.scale;
        for (i, hit) in self.ui.tabs.iter().enumerate() {
            let r = hit.rect;
            let active = i == self.active;
            let hover = r.contains(mx, my);
            if active {
                g.round(r, 8.0 * s, BG, 1.0);
                g.fill(Rect::new(r.x, r.y + r.h / 2.0, r.w, r.h / 2.0 + 1.0), BG);
            } else if hover {
                g.round(Rect::new(r.x, r.y, r.w, r.h - 4.0 * s), 8.0 * s, TAB_HOVER, 1.0);
            } else if i + 1 != self.active && i + 1 < self.tabs.len() {
                g.fill(Rect::new(r.right(), r.y + 9.0 * s, 1.0, r.h - 22.0 * s), BORDER);
            }

            // Status dot.
            let status = self.tab_status(i);
            let alert = self.tabs[i].alert;
            let dot = match (alert, status) {
                (Alert::Asking, _) | (_, "asking") => Some(RED),
                (Alert::Done, _) => Some(GREEN),
                (_, "busy") => Some(AMBER),
                (_, "idle") => Some(GREEN.mix(BG, 0.35)),
                (_, "sleeping") => Some(BLUE.mix(BG, 0.2)),
                _ => None,
            };
            let cy = r.y + (r.h - if active { 0.0 } else { 4.0 * s }) / 2.0;
            let mut tx = r.x + 12.0 * s;
            if let Some(c) = dot {
                if alert != Alert::None {
                    g.circle(tx + 4.0 * s, cy, 7.0 * s, c.mix(BAR, 0.65));
                }
                if status == "sleeping" {
                    g.circle(tx + 4.0 * s, cy, 4.0 * s, c);
                    g.circle(tx + 4.0 * s, cy, 2.4 * s, if active { BG } else { BAR });
                } else {
                    g.circle(tx + 4.0 * s, cy, 4.0 * s, c);
                }
                tx += 16.0 * s;
            }

            // RAM badge.
            let mem = self.tab_mem(i);
            let warn = self.cfg.ram_warn_mb * 1_048_576;
            let mut text_end = hit.close.x - 4.0 * s;
            if mem > 0 {
                let mut label = fmt_bytes(mem).replace(" GB", "G").replace(" MB", "M");
                if self.refresh_hint(i) {
                    label = format!("↻ {label}");
                }
                let (fg, bg) = if mem >= warn * 2 {
                    (RED, Some(RED))
                } else if mem >= warn {
                    (AMBER, Some(AMBER))
                } else {
                    (TEXT_DIM, None)
                };
                let b = Rect::new(hit.badge.x, cy - hit.badge.h / 2.0, hit.badge.w, hit.badge.h);
                if let Some(c) = bg {
                    g.round(b, b.h / 2.0, c, 0.18);
                }
                g.text(&label, &g.ui_center_small(), b, fg);
                text_end = b.x - 4.0 * s;
            }

            let mut title = self.tab_title(i);
            if self.tabs[i].zoom.is_some() {
                title = format!("⤢ {title}");
            }
            let title_r = Rect::new(tx, r.y, (text_end - tx).max(10.0), r.h - if active { 0.0 } else { 4.0 * s });
            if let Some((ri, text)) = &self.ui.rename {
                if *ri == i {
                    let er = Rect::new(tx - 4.0 * s, cy - 11.0 * s, (hit.close.right() - tx).max(40.0), 22.0 * s);
                    g.round(er, 5.0 * s, BAR, 1.0);
                    g.outline(er, 5.0 * s, ACCENT, 1.0);
                    g.text(&format!("{text}▏"), &g.ui, Rect::new(er.x + 6.0 * s, er.y, er.w - 8.0 * s, er.h), TEXT);
                    continue;
                }
            }
            let tc = if active { TEXT } else { TEXT_DIM.mix(TEXT, if hover { 0.5 } else { 0.0 }) };
            g.text(&title, if active || alert != Alert::None { &g.ui_bold } else { &g.ui }, title_r, tc);

            if active || hover {
                let c = hit.close;
                let c = Rect::new(c.x, cy - c.h / 2.0, c.w, c.h);
                if c.contains(mx, my) {
                    g.round(c, 4.0 * s, BORDER, 1.0);
                }
                let k = 4.0 * s;
                let col = if c.contains(mx, my) { TEXT } else { TEXT_DIM };
                g.line(c.cx() - k, c.cy() - k, c.cx() + k, c.cy() + k, 1.3 * s, col);
                g.line(c.cx() + k, c.cy() - k, c.cx() - k, c.cy() + k, 1.3 * s, col);
            }
        }

        // + and profile arrow.
        for (r, plus) in [(self.ui.plus, true), (self.ui.menu, false)] {
            if r.contains(mx, my) {
                g.round(r, 6.0 * s, TAB_HOVER, 1.0);
            }
            let (cx, cy) = (r.cx(), r.cy() - 2.0 * s);
            if plus {
                let k = 6.0 * s;
                g.line(cx - k, cy, cx + k, cy, 1.5 * s, TEXT_DIM);
                g.line(cx, cy - k, cx, cy + k, 1.5 * s, TEXT_DIM);
            } else {
                let k = 3.5 * s;
                g.line(cx - k, cy - k / 2.0, cx, cy + k / 2.0, 1.4 * s, TEXT_DIM);
                g.line(cx, cy + k / 2.0, cx + k, cy - k / 2.0, 1.4 * s, TEXT_DIM);
            }
        }

        // System RAM meter on the right.
        if self.sys.total > 0 {
            let r = self.ui.sys;
            let used = self.sys.total - self.sys.avail;
            let frac = used as f32 / self.sys.total as f32;
            let c = if frac >= 0.9 { RED } else if frac >= 0.78 { AMBER } else { GREEN };
            let label = format!("RAM {:.1}/{:.1} GB", used as f64 / 1.073_741_824e9, self.sys.total as f64 / 1.073_741_824e9);
            let cy = r.y + (r.h - 4.0 * s) / 2.0;
            g.text(&label, &g.ui_small, Rect::new(r.x, cy - 10.0 * s, r.w, 14.0 * s), TEXT_DIM);
            let bar = Rect::new(r.x, cy + 6.0 * s, r.w - 6.0 * s, 3.0 * s);
            g.round(bar, 1.5 * s, BORDER, 1.0);
            g.round(Rect::new(bar.x, bar.y, (bar.w * frac).max(2.0), bar.h), 1.5 * s, c, 1.0);
        }
    }

    fn paint_toast(&self) {
        let Some((text, _)) = &self.toast else { return };
        let g = &self.gfx;
        let s = g.scale;
        let (w, h) = self.client();
        let tw = (g.measure(text, &g.ui) + 40.0 * s).min(w - 40.0 * s);
        let r = Rect::new((w - tw) / 2.0, h - 64.0 * s, tw, 36.0 * s);
        g.round(r, r.h / 2.0, CARD, 0.98);
        g.outline(r, r.h / 2.0, ACCENT.mix(BORDER, 0.5), 1.0);
        g.text(text, &g.ui_center, r, TEXT);
    }

    fn paint_tooltip(&self) {
        let (mx, my) = self.ui.mouse;
        if !matches!(self.ui.drag, Drag::None) || my > self.bar_h() {
            return;
        }
        let mut lines: Vec<String> = Vec::new();
        if self.ui.sys.contains(mx, my) && self.sys.total > 0 {
            let gb = |b: u64| b as f64 / 1.073_741_824e9;
            let tabs: u64 = (0..self.tabs.len()).map(|i| self.tab_mem(i)).sum();
            lines.push(format!("Free RAM: {:.1} GB of {:.1} GB", gb(self.sys.avail), gb(self.sys.total)));
            lines.push(format!("Committed: {:.1} of {:.1} GB (RAM + page file)", gb(self.sys.commit_used), gb(self.sys.commit_limit)));
            lines.push(format!("Programs in your tabs: {}", fmt_bytes(tabs)));
            lines.push(format!("PulseTerm itself: {}", fmt_bytes(self.sys.self_mem)));
        } else if let Some(i) = self.ui.tabs.iter().position(|t| t.badge.contains(mx, my) || t.rect.contains(mx, my)) {
            if !self.ui.tabs[i].badge.inset(-4.0).contains(mx, my) {
                return;
            }
            for id in self.tabs[i].root.leaves() {
                if let Some(p) = self.panes.get(&id) {
                    for (name, b) in &p.stats.top {
                        lines.push(format!("{name}  {}", fmt_bytes(*b)));
                    }
                }
            }
            if self.refresh_hint(i) {
                lines.push(format!(
                    "Idle for a while. Click this badge to refresh the chat (frees about {}).",
                    fmt_bytes(self.refresh_gain(i))
                ));
            } else if self.tab_mem(i) >= self.cfg.ram_warn_mb * 1_048_576 {
                lines.push("Right-click the tab > Refresh chat or Sleep tab to free it.".into());
            }
        }
        if lines.is_empty() {
            return;
        }
        let g = &self.gfx;
        let s = g.scale;
        let w = lines.iter().map(|l| g.measure(l, &g.ui)).fold(0.0, f32::max) + 24.0 * s;
        let h = lines.len() as f32 * 20.0 * s + 14.0 * s;
        let (cw, _) = self.client();
        let x = (mx - w / 2.0).clamp(6.0, cw - w - 6.0);
        let r = Rect::new(x, self.bar_h() + 4.0 * s, w, h);
        g.round(r, 8.0 * s, CARD, 0.98);
        g.outline(r, 8.0 * s, BORDER, 1.0);
        for (k, l) in lines.iter().enumerate() {
            g.text(l, &g.ui, Rect::new(r.x + 12.0 * s, r.y + 7.0 * s + k as f32 * 20.0 * s, w - 20.0 * s, 20.0 * s), TEXT);
        }
    }
}

impl Gfx {
    fn ui_center_small(&self) -> windows::Win32::Graphics::DirectWrite::IDWriteTextFormat {
        unsafe {
            let f = self.ui_small.clone();
            let _ = f.SetTextAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_CENTER);
            f
        }
    }
}

/// Split a row into runs we can shape once and draw many times.
fn build_row(g: &Gfx, cells: &[VCell]) -> RowCache {
    let mut out = RowCache::default();
    #[derive(PartialEq, Clone, Copy)]
    struct Style {
        fg: Rgb,
        fmt: usize,
        ul: bool,
        strike: bool,
        main_font: bool,
    }
    let mut buf: Vec<u16> = Vec::new();
    let mut start = 0usize;
    let mut cur: Option<Style> = None;
    let flush = |buf: &mut Vec<u16>, start: usize, st: Option<Style>, out: &mut RowCache| {
        if let Some(st) = st {
            let mut end = buf.len();
            if !st.ul && !st.strike {
                while end > 0 && buf[end - 1] == b' ' as u16 {
                    end -= 1;
                }
            }
            if end > 0 {
                if let Some(layout) = g.make_layout(&buf[..end], st.fmt, st.ul, st.strike) {
                    let special = (!st.main_font).then(|| (buf[..end].to_vec(), st.fmt, st.ul, st.strike));
                    out.segs.push(Seg { col: start, layout, color: st.fg, special });
                }
            }
        }
        buf.clear();
    };
    for (col, cell) in cells.iter().enumerate() {
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let fg = cell.fg;
        let c = cell.c;
        if is_box(c) {
            flush(&mut buf, start, cur.take(), &mut out);
            out.boxes.push((col, c, fg));
            continue;
        }
        let fl = cell.flags;
        let fmt = (fl.contains(Flags::BOLD) as usize) | ((fl.contains(Flags::ITALIC) as usize) << 1);
        let ul = fl.intersects(Flags::ALL_UNDERLINES);
        let strike = fl.contains(Flags::STRIKEOUT);
        let wide = fl.contains(Flags::WIDE_CHAR);
        let zw = cell.zw.as_deref();
        let main_font = !wide && g.has_glyph(c) && zw.is_none_or(|z| z.iter().all(|x| g.has_glyph(*x)));
        if c == ' ' && !ul && !strike && cur.is_some_and(|s| s.main_font) {
            // Spaces join the current run (keeps runs long and cheap).
            buf.push(b' ' as u16);
            continue;
        }
        let st = Style { fg, fmt, ul, strike, main_font };
        // A character from another font (⏵ ⎿ ✻, emoji) gets its own piece. That font's
        // letters are often wider than our cell, so two in a row would push the second one
        // into the next column (⏵⏵ came out as "⏵ ⏵").
        let alone = wide || !main_font;
        if alone || cur != Some(st) {
            flush(&mut buf, start, cur.take(), &mut out);
            start = col;
            cur = Some(st);
        }
        let mut tmp = [0u16; 2];
        buf.extend_from_slice(c.encode_utf16(&mut tmp));
        if let Some(z) = zw {
            for x in z {
                buf.extend_from_slice(x.encode_utf16(&mut tmp));
            }
        }
        if alone {
            flush(&mut buf, start, cur.take(), &mut out);
        }
    }
    flush(&mut buf, start, cur.take(), &mut out);
    out
}
