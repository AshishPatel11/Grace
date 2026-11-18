//! Selecting text while a full-screen program (Claude) scrolls.
//!
//! In full-screen mode the terminal only holds the rows you can see. When Claude scrolls its
//! chat, it draws every row again one row higher or lower; the terminal is never told "the
//! text moved". So a selection stayed on the same screen rows while the text slid away under
//! it, and rows that left the screen were gone (copy lost them).
//!
//! What Grace does instead, once per frame while something is selected:
//! 1. Compare the rows with the last frame to see how far the text moved (`shift`).
//! 2. Keep the selection's two ends in "virtual rows" (screen row + `base`, where `base`
//!    grows when the text moves up), so the selection moves with the text, even off screen.
//! 3. Keep the text of selected rows that left the screen, so copying still gets them.
//! 4. While you drag, the moving end follows the mouse.

use std::collections::HashMap;

use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VPoint {
    /// Virtual row: screen row + `base` at the time.
    pub row: i64,
    pub col: usize,
    pub side: Side,
}

#[derive(Clone, Debug, PartialEq)]
struct VSel {
    ty: SelectionType,
    /// Where the selection started.
    a: VPoint,
    /// The end that moves while you drag.
    b: VPoint,
}

impl VSel {
    fn ordered(&self) -> (VPoint, VPoint) {
        if (self.a.row, self.a.col) <= (self.b.row, self.b.col) {
            (self.a, self.b)
        } else {
            (self.b, self.a)
        }
    }
}

/// One screen of rows, as `frame` needs them.
pub struct Screen<'a> {
    /// Content hash of each row; None for an empty row (empty rows match anything).
    pub hashes: &'a [Option<u64>],
    /// The characters of each row, one per column ('\0' for the second half of a wide one).
    pub cells: &'a [Vec<char>],
    /// Rows from here down never scroll (Claude's input box and status lines).
    pub end: usize,
}

#[derive(Default)]
pub struct SelMem {
    prev: Vec<Option<u64>>,
    prev_cells: Vec<Vec<char>>,
    /// Virtual row of screen row 0.
    base: i64,
    sel: Option<VSel>,
    /// The selection last put into the terminal, to notice when something else changed it.
    shown: Option<Selection>,
    /// Text of selected virtual rows that left the screen.
    gone: HashMap<i64, Vec<char>>,
    /// The chat area in the last frame: rows above this scroll.
    end: usize,
    /// You cleared the selection (a click, typing, Ctrl+C). Any other way it disappears is
    /// the program erasing a selected row (the terminal then drops it): it comes back.
    forgotten: std::cell::Cell<bool>,
}

/// How many rows the text moved up (positive) or down (negative) since the last frame:
/// the move that lines up the most rows. Ties keep 0 (Claude's input box never moves).
pub fn shift(prev: &[Option<u64>], cur: &[Option<u64>]) -> i32 {
    let n = prev.len().min(cur.len()) as i32;
    let score = |d: i32| (0..n).filter(|&i| cur[i as usize].is_some() && (0..n).contains(&(i + d)) && cur[i as usize] == prev[(i + d) as usize]).count();
    let (mut best, mut at) = (score(0), 0);
    for d in 1..n {
        for d in [d, -d] {
            let s = score(d);
            if s > best {
                (best, at) = (s, d);
            }
        }
    }
    if best >= 2 {
        at
    } else {
        0
    }
}

impl SelMem {
    /// Forget everything (a new selection starts, or the program left full-screen mode).
    pub fn reset(&mut self) {
        *self = SelMem::default();
    }

    pub fn active(&self) -> bool {
        self.sel.is_some()
    }

    /// You cleared the selection: forget it at the next frame.
    pub fn forget(&self) {
        self.forgotten.set(true);
    }

    /// You pressed the mouse to start a selection at screen point `pt`. From now on the
    /// moving end follows the mouse (see `frame`).
    pub fn start(&mut self, ty: SelectionType, pt: Point, side: Side) {
        self.gone.clear();
        let v = VPoint { row: pt.line.0 as i64 + self.base, col: pt.column.0, side };
        self.sel = Some(VSel { ty, a: v, b: v });
        self.shown = None;
        self.forgotten.set(false);
    }

    /// Once per frame. `current`: the terminal's selection and its range on screen.
    /// `drag`: while you drag, the screen point under the mouse (row, column, side).
    /// Returns the selection to put into the terminal, when it must change.
    pub fn frame(
        &mut self,
        screen: Screen,
        current: Option<&Selection>,
        current_range: Option<(Point, Point)>,
        drag: Option<(usize, usize, Side)>,
    ) -> Option<Option<Selection>> {
        if self.forgotten.take() {
            self.reset();
        }
        let n = screen.end.min(screen.hashes.len());
        let cols = screen.cells.first().map_or(1, |c| c.len().max(1));
        let mut restore = false;
        match (current, &self.sel) {
            // Gone, but you did not clear it: the program erased a selected row.
            (None, Some(_)) => restore = true,
            (None, None) => {
                self.reset();
                return None;
            }
            (Some(cur), _) if self.sel.is_none() || (drag.is_none() && self.shown.is_some() && Some(cur) != self.shown.as_ref()) => {
                // New, or changed by something else (keyboard, a click): start from what is on screen.
                let Some((s, e)) = current_range else {
                    self.reset();
                    return None;
                };
                self.gone.clear();
                let p = |pt: Point, side| VPoint { row: pt.line.0 as i64 + self.base, col: pt.column.0, side };
                let (a, b) = (p(s, Side::Left), p(e, Side::Right));
                self.sel = Some(VSel { ty: cur.ty, a, b });
                self.shown = Some(cur.clone());
                if drag.is_none() {
                    self.keep(screen);
                    return None;
                }
            }
            _ => {}
        }

        // How far did the text move? Keep the selected rows that leave the screen.
        let d = if self.prev.len() == screen.hashes.len() { shift(&self.prev[..n], &screen.hashes[..n]) } else { 0 };
        if d != 0 {
            let leaving: Vec<usize> = if d > 0 { (0..(d as usize).min(n)).collect() } else { (n.saturating_sub((-d) as usize)..n).collect() };
            for i in leaving {
                if let Some(c) = self.prev_cells.get(i) {
                    self.gone.insert(self.base + i as i64, c.clone());
                }
            }
            self.base += d as i64;
        }
        let base = self.base;
        let sel = self.sel.as_mut().unwrap();
        if let Some((row, col, side)) = drag {
            sel.b = VPoint { row: row as i64 + base, col, side };
        }
        let (lo, hi) = sel.ordered();
        self.gone.retain(|r, _| (lo.row..=hi.row).contains(r));

        // Put it back into the terminal, in screen rows (they may be off the screen;
        // the terminal clamps them when drawing).
        // Below the chat area (into the input box) it stops at the chat's last row.
        let at = |v: VPoint| match v.row - base {
            r if r >= n as i64 => (Point::new(Line(n as i32 - 1), Column(cols - 1)), Side::Right),
            r => (Point::new(Line(r as i32), Column(v.col)), v.side),
        };
        let (pa, sa) = at(sel.a);
        let (pb, sb) = at(sel.b);
        let mut s = Selection::new(sel.ty, pa, sa);
        s.update(pb, sb);
        self.keep(screen);
        if restore || d != 0 || Some(&s) != current {
            self.shown = Some(s.clone());
            return Some(Some(s));
        }
        None
    }

    fn keep(&mut self, screen: Screen) {
        self.end = screen.end.min(screen.hashes.len());
        self.prev = screen.hashes.to_vec();
        self.prev_cells = screen.cells.to_vec();
    }

    /// The whole selection is below the chat area (the terminal would still show one cell).
    pub fn below_screen(&self) -> bool {
        self.sel.as_ref().is_some_and(|s| s.ordered().0.row - self.base >= self.end as i64)
    }

    /// The selected text, when part of it is off the screen. None = it is all on screen
    /// (the terminal's own copy is better then: it knows words and wrapped lines).
    pub fn text(&self, screen: &[Vec<char>]) -> Option<String> {
        let sel = self.sel.as_ref()?;
        let (lo, hi) = sel.ordered();
        let n = self.end.min(screen.len()) as i64;
        if lo.row - self.base >= 0 && hi.row - self.base < n {
            return None;
        }
        let whole = sel.ty == SelectionType::Lines;
        let mut out: Vec<String> = Vec::new();
        for v in lo.row..=hi.row {
            let r = v - self.base;
            let cells: &[char] = if (0..n).contains(&r) { &screen[r as usize] } else { self.gone.get(&v).map_or(&[], |c| c.as_slice()) };
            let from = if v == lo.row && !whole { lo.col + (lo.side == Side::Right) as usize } else { 0 };
            let to = if v == hi.row && !whole { (hi.col + 1).saturating_sub((hi.side == Side::Left) as usize) } else { cells.len() };
            let line: String = cells.iter().take(to.min(cells.len())).skip(from).filter(|c| **c != '\0').collect();
            out.push(line.trim_end().to_string());
        }
        Some(out.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen from text rows: hash = the text, empty rows = None.
    fn screen(rows: &[&str]) -> (Vec<Option<u64>>, Vec<Vec<char>>) {
        let hashes = rows
            .iter()
            .map(|r| (!r.trim().is_empty()).then(|| r.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211))))
            .collect();
        let cells = rows.iter().map(|r| format!("{r:<20}").chars().collect()).collect();
        (hashes, cells)
    }

    fn pt(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    fn sel(a: Point, b: Point) -> Selection {
        let mut s = Selection::new(SelectionType::Simple, a, Side::Left);
        s.update(b, Side::Right);
        s
    }

    /// Feed one frame; returns the terminal's selection afterwards.
    /// The last row is Claude's input line: it never scrolls.
    fn step(m: &mut SelMem, rows: &[&str], cur: Option<Selection>, drag: Option<(usize, usize, Side)>) -> Option<Selection> {
        let (h, c) = screen(rows);
        let range = cur.as_ref().map(guess_range);
        match m.frame(Screen { hashes: &h, cells: &c, end: rows.len() - 1 }, cur.as_ref(), range, drag) {
            Some(new) => new,
            None => cur,
        }
    }

    /// Our tests only build simple selections on screen; their range is their two ends.
    fn guess_range(s: &Selection) -> (Point, Point) {
        let dbg = format!("{s:?}");
        let nums: Vec<i64> = dbg
            .split(|c: char| !c.is_ascii_digit() && c != '-')
            .filter_map(|x| x.parse().ok())
            .collect();
        // Debug order: start line, start column, end line, end column.
        let (a, b) = (pt(nums[0] as i32, nums[1] as usize), pt(nums[2] as i32, nums[3] as usize));
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    }

    #[test]
    fn shift_finds_how_far_the_text_moved() {
        let a = screen(&["one", "two", "three", "four", "", "> input"]).0;
        let b = screen(&["three", "four", "five", "six", "", "> input"]).0;
        assert_eq!(shift(&a, &b), 2);
        assert_eq!(shift(&b, &a), -2);
        assert_eq!(shift(&a, &a), 0);
        // Only the input line changed: nothing moved.
        let c = screen(&["one", "two", "three", "four", "", "> inp"]).0;
        assert_eq!(shift(&a, &c), 0);
    }

    #[test]
    fn the_selection_moves_with_the_text_and_copy_keeps_rows_that_left() {
        let mut m = SelMem::default();
        let first = ["alpha", "beta", "gamma", "delta", "", "> input"];
        // Select "beta" .. "gamma" (rows 1-2).
        let s = step(&mut m, &first, Some(sel(pt(1, 0), pt(2, 19))), None);
        // Claude scrolls by 2: beta is gone off the top, gamma is now row 0.
        let s = step(&mut m, &["gamma", "delta", "eps", "zeta", "", "> input"], s, None).unwrap();
        let (_, c) = screen(&["gamma", "delta", "eps", "zeta", "", "> input"]);
        assert_eq!(guess_range(&s), (pt(-1, 0), pt(0, 19)));
        assert_eq!(m.text(&c).as_deref(), Some("beta\ngamma"));
    }

    #[test]
    fn dragging_while_scrolling_keeps_the_start() {
        let mut m = SelMem::default();
        let rows = ["l1", "l2", "l3", "l4", "l5", "> in"];
        // Drag starts on l4 (row 3) and goes up to the top row.
        let s = step(&mut m, &rows, Some(sel(pt(3, 2), pt(3, 2))), Some((3, 2, Side::Left)));
        let s = step(&mut m, &rows, s, Some((0, 0, Side::Left)));
        // The mouse stays above the chat; Claude scrolls up (older text comes in at the top).
        let s = step(&mut m, &["l0", "l1", "l2", "l3", "l4", "> in"], s, Some((0, 0, Side::Left)));
        let s = step(&mut m, &["k9", "l0", "l1", "l2", "l3", "> in"], s, Some((0, 0, Side::Left))).unwrap();
        let (_, c) = screen(&["k9", "l0", "l1", "l2", "l3", "> in"]);
        // From k9 down to "l4" (now below the screen), up to where the drag started.
        assert_eq!(m.text(&c).as_deref(), Some("k9\nl0\nl1\nl2\nl3\nl4"));
        assert!(!m.below_screen());
        let _ = s;
    }

    #[test]
    fn only_you_clear_the_selection() {
        let mut m = SelMem::default();
        let rows = ["a1", "a2", "a3", "a4"];
        step(&mut m, &rows, Some(sel(pt(0, 0), pt(1, 1))), None);
        // The program erased a selected row and the terminal dropped the selection: it comes back.
        assert!(step(&mut m, &rows, None, None).is_some());
        // A click clears it for real.
        m.forget();
        assert!(step(&mut m, &rows, None, None).is_none());
        assert!(!m.active());
    }
}
