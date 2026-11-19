//! The diff viewer: what changed in a tab's git folder, shown inside Grace.
//!
//! Click "Changes" on the toolbar. On the left, the changed files; on the right, what changed
//! in the chosen file since the last commit: removed lines in red, added lines in green, with
//! the old and new line numbers. ↑↓, the wheel, PgUp/PgDn scroll; ←→ or a click on the left
//! switch files; Esc or a click outside closes it.
//!
//! git runs only when the viewer opens or switches files (see changes.rs), never in the
//! background.

use std::path::{Path, PathBuf};

use crate::app::App;
use crate::changes::{changed_files, Changed};
use crate::layout::Rect;
use crate::theme::*;
use crate::win::wstr;

/// Lines kept for one file (a huge generated file would only be slow to look at).
const MAX_LINES: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Same,
    Added,
    Removed,
    /// "@@ -10,7 +10,9 @@ fn main": where the next lines are.
    Hunk,
    /// "No newline at end of file", "Binary file", ...
    Note,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DLine {
    pub kind: Kind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

fn line(kind: Kind, old: Option<u32>, new: Option<u32>, text: &str) -> DLine {
    // Tabs as 4 spaces; the terminal font has no tab stops.
    DLine { kind, old, new, text: text.trim_end_matches('\r').replace('\t', "    ") }
}

/// `git diff` output (one file) as lines with their old and new line numbers.
pub fn parse_unified(diff: &str) -> Vec<DLine> {
    let mut out = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    for l in diff.lines() {
        if let Some(rest) = l.strip_prefix("@@ ") {
            // "@@ -a,b +c,d @@ context"
            let mut nums = rest.split_whitespace();
            let start = |s: Option<&str>| s.and_then(|x| x[1..].split(',').next()).and_then(|x| x.parse::<u32>().ok()).unwrap_or(1);
            old = start(nums.next());
            new = start(nums.next());
            in_hunk = true;
            out.push(line(Kind::Hunk, None, None, l));
            continue;
        }
        if l.starts_with("Binary files") {
            out.push(line(Kind::Note, None, None, "Binary file: it changed, but it is not text."));
            continue;
        }
        if !in_hunk {
            continue; // "diff --git", "index", "---", "+++"
        }
        match l.as_bytes().first() {
            Some(b'+') => {
                out.push(line(Kind::Added, None, Some(new), &l[1..]));
                new += 1;
            }
            Some(b'-') => {
                out.push(line(Kind::Removed, Some(old), None, &l[1..]));
                old += 1;
            }
            Some(b'\\') => out.push(line(Kind::Note, None, None, l.trim_start_matches("\\ "))),
            _ => {
                out.push(line(Kind::Same, Some(old), Some(new), l.get(1..).unwrap_or("")));
                old += 1;
                new += 1;
            }
        }
        if out.len() >= MAX_LINES {
            out.push(line(Kind::Note, None, None, "… the rest is not shown (very long)."));
            break;
        }
    }
    out
}

/// A new file: every line is added.
pub fn new_file_lines(text: &str) -> Vec<DLine> {
    if text.contains('\0') {
        return vec![line(Kind::Note, None, None, "Binary file: new, not text.")];
    }
    let mut out: Vec<DLine> = text.lines().take(MAX_LINES).enumerate().map(|(i, l)| line(Kind::Added, None, Some(i as u32 + 1), l)).collect();
    if out.is_empty() {
        out.push(line(Kind::Note, None, None, "Empty file."));
    }
    out
}

fn git_diff(root: &Path, path: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "HEAD", "--no-color", "--no-ext-diff", "-U3", "--", path])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The lines to show for one changed file.
fn load(root: &Path, f: &Changed) -> Vec<DLine> {
    if f.status == "??" {
        return match std::fs::read(root.join(&f.path)) {
            Ok(b) => new_file_lines(&String::from_utf8_lossy(&b)),
            Err(_) => vec![line(Kind::Note, None, None, "Could not read this file.")],
        };
    }
    match git_diff(root, &f.path) {
        Some(d) if !d.trim().is_empty() => parse_unified(&d),
        Some(_) => vec![line(Kind::Note, None, None, "No changes in the text (only its mode or line endings).")],
        None => vec![line(Kind::Note, None, None, "git could not show this file (no commit yet?).")],
    }
}

pub struct DiffView {
    pub root: PathBuf,
    pub files: Vec<Changed>,
    pub cur: usize,
    pub lines: Vec<DLine>,
    /// First line shown.
    pub top: usize,
    /// Lines that fit (from the last frame).
    pub page: usize,
    pub card: Rect,
    /// File list rows drawn in the last frame: (rect, file index).
    pub rows: Vec<(Rect, usize)>,
}

impl DiffView {
    fn scroll(&mut self, by: i64) {
        let max = self.lines.len().saturating_sub(self.page.max(1)) as i64;
        self.top = (self.top as i64 + by).clamp(0, max.max(0)) as usize;
    }
}

impl App {
    /// Click on "Changes": open the viewer on the first changed file.
    pub fn open_diff_view(&mut self, id: u64) {
        let Some(root) = self.pane_repo(id) else { return };
        let Some(files) = changed_files(&root) else {
            self.show_toast("git did not answer here (is git installed?).".into());
            return;
        };
        if files.is_empty() {
            self.show_toast("Nothing changed since the last commit.".into());
            return;
        }
        let lines = load(&root, &files[0]);
        self.ui.diff = Some(DiffView { root, files, cur: 0, lines, top: 0, page: 30, card: Rect::default(), rows: Vec::new() });
        self.invalidate();
    }

    fn diff_show(&mut self, i: usize) {
        let Some(v) = &mut self.ui.diff else { return };
        if i >= v.files.len() || (i == v.cur && !v.lines.is_empty()) {
            return;
        }
        v.cur = i;
        v.lines = load(&v.root, &v.files[i]);
        v.top = 0;
        self.invalidate();
    }

    /// Keys while the viewer is open: all of them stay here.
    pub fn diff_key(&mut self, vk: u16) -> bool {
        use crate::input::*;
        let Some(v) = &mut self.ui.diff else { return false };
        let page = v.page.max(1) as i64;
        match vk {
            VK_ESCAPE => self.ui.diff = None,
            VK_DOWN => v.scroll(1),
            VK_UP => v.scroll(-1),
            VK_NEXT | VK_SPACE => v.scroll(page - 2),
            VK_PRIOR => v.scroll(-(page - 2)),
            VK_HOME => v.top = 0,
            VK_END => v.scroll(i64::MAX / 4),
            VK_RIGHT | VK_TAB => {
                let next = (v.cur + 1) % v.files.len();
                self.diff_show(next);
            }
            VK_LEFT => {
                let prev = (v.cur + v.files.len() - 1) % v.files.len();
                self.diff_show(prev);
            }
            _ => {}
        }
        self.invalidate();
        true
    }

    pub fn diff_mouse_down(&mut self, x: f32, y: f32) {
        let Some(v) = &self.ui.diff else { return };
        if let Some((_, i)) = v.rows.iter().find(|(r, _)| r.contains(x, y)) {
            let i = *i;
            self.diff_show(i);
        } else if !v.card.contains(x, y) {
            self.ui.diff = None;
        }
        self.invalidate();
    }

    pub fn diff_wheel(&mut self, delta: i16) -> bool {
        let Some(v) = &mut self.ui.diff else { return false };
        v.scroll(if delta > 0 { -3 } else { 3 });
        self.invalidate();
        true
    }

    pub fn paint_diff(&mut self) {
        let Some(mut v) = self.ui.diff.take() else { return };
        let g = &self.gfx;
        let s = g.scale;
        let (w, h) = self.client();
        g.fill_a(Rect::new(0.0, 0.0, w, h), BAR, 0.6);
        let card = Rect::new((20.0 * s).round(), (self.bar_h() + 12.0 * s).round(), w - 40.0 * s, h - self.bar_h() - 32.0 * s);
        v.card = card;
        g.round(card, 10.0 * s, CARD, 1.0);
        g.outline(card, 10.0 * s, BORDER, 1.0);
        let pad = 14.0 * s;

        // Top line: what this is, and the keys.
        let head_h = (40.0 * s).round();
        let repo = crate::folders::short_name(&v.root.to_string_lossy());
        let n = v.files.len();
        let title = format!("Changes in {repo} since the last commit · {n} file{}", if n == 1 { "" } else { "s" });
        g.text(&title, &g.ui_bold, Rect::new(card.x + pad, card.y, card.w / 2.0, head_h), TEXT);
        let keys = "←→ file · ↑↓ wheel PgUp PgDn scroll · Esc close";
        let kw = g.measure(keys, &g.ui_small);
        g.text(keys, &g.ui_small, Rect::new(card.right() - pad - kw, card.y, kw + 2.0, head_h), TEXT_DIM);
        g.fill(Rect::new(card.x, card.y + head_h, card.w, 1.0), BORDER);

        // Left: the files.
        let list_w = (card.w * 0.28).clamp(200.0 * s, 340.0 * s).round();
        let list = Rect::new(card.x, card.y + head_h + 1.0, list_w, card.h - head_h - 1.0);
        g.fill(Rect::new(list.right(), list.y, 1.0, list.h), BORDER);
        let row_h = (28.0 * s).round();
        let fits = ((list.h - 8.0 * s) / row_h).floor().max(1.0) as usize;
        let first = v.cur.saturating_sub(fits.saturating_sub(1));
        v.rows.clear();
        let (mx, my) = self.ui.mouse;
        for (k, i) in (first..n).take(fits).enumerate() {
            let f = &v.files[i];
            let r = Rect::new(list.x + 6.0 * s, list.y + 4.0 * s + k as f32 * row_h, list.w - 12.0 * s, row_h - 2.0 * s);
            if i == v.cur {
                g.round(r, 5.0 * s, SELECTION, 0.7);
            } else if r.contains(mx, my) {
                g.round(r, 5.0 * s, TAB_HOVER, 1.0);
            }
            let (letter, lc) = match f.letter() {
                'A' => ("A", GREEN),
                'D' => ("D", RED),
                'R' => ("R", BLUE),
                _ => ("M", AMBER),
            };
            g.text(letter, &g.ui_bold, Rect::new(r.x + 8.0 * s, r.y, 14.0 * s, r.h), lc);
            let counts = match f.lines {
                Some((a, d)) => format!("+{a} −{d}"),
                None if f.letter() == 'A' => "new".into(),
                None => String::new(),
            };
            let cw = g.measure(&counts, &g.ui_small);
            g.text(&counts, &g.ui_small, Rect::new(r.right() - cw - 8.0 * s, r.y, cw + 2.0, r.h), TEXT_DIM);
            let name = Path::new(&f.path).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| f.path.clone());
            let nx = r.x + 26.0 * s;
            let room = r.right() - cw - 16.0 * s - nx;
            let nw = g.measure(&name, &g.ui).min(room);
            g.text(&name, &g.ui, Rect::new(nx, r.y, nw + 2.0, r.h), if i == v.cur { TEXT } else { TEXT.mix(TEXT_DIM, 0.3) });
            // The folder part, dimmer, after the name.
            if let Some(dir) = f.path.rsplit_once('/').map(|(d, _)| d) {
                let dx = nx + nw + 6.0 * s;
                if room - nw > 30.0 * s {
                    g.text(dir, &g.ui_small, Rect::new(dx, r.y, room - nw - 6.0 * s, r.h), TEXT_DIM);
                }
            }
            v.rows.push((r, i));
        }

        // Right: the chosen file's lines.
        let body = Rect::new(list.right() + 1.0, list.y, card.right() - list.right() - 1.0, list.h);
        let f = v.files[v.cur].clone();
        let file_h = (32.0 * s).round();
        let label = match f.lines {
            Some((a, d)) => format!("{}   +{a} −{d}", f.path),
            None => f.path.clone(),
        };
        g.text(&label, &g.ui_bold, Rect::new(body.x + pad, body.y, body.w - 2.0 * pad, file_h), TEXT);
        let text_area = Rect::new(body.x, body.y + file_h, body.w - 10.0 * s, body.h - file_h - 8.0 * s);
        let lh = g.cell_h.round();
        v.page = (text_area.h / lh).floor().max(1.0) as usize;
        v.scroll(0);
        let cw = g.cell_w;
        let num_w = 6.0 * cw;
        g.clip(text_area);
        for (k, d) in v.lines.iter().skip(v.top).take(v.page).enumerate() {
            let y = text_area.y + k as f32 * lh;
            let row = Rect::new(text_area.x, y, text_area.w, lh);
            let (bg, sign, sign_c) = match d.kind {
                Kind::Added => (Some(GREEN.mix(CARD, 0.84)), "+", GREEN),
                Kind::Removed => (Some(RED.mix(CARD, 0.84)), "−", RED),
                Kind::Hunk => (Some(CODE_HEAD), "", TEXT_DIM),
                _ => (None, "", TEXT_DIM),
            };
            if let Some(b) = bg {
                g.fill(row, b);
            }
            let num = |n: Option<u32>| n.map(|x| x.to_string()).unwrap_or_default();
            let x0 = text_area.x + 8.0 * s;
            for (n, x) in [(num(d.old), x0), (num(d.new), x0 + num_w)] {
                if !n.is_empty() {
                    if let Some(l) = g.make_layout(&wstr(&n), 0, false, false) {
                        let nx = x + num_w - (n.chars().count() as f32 + 1.0) * cw;
                        g.draw_layout(&l, nx, y, TEXT_DIM.mix(CARD, 0.3));
                    }
                }
            }
            let tx = x0 + 2.0 * num_w;
            if !sign.is_empty() {
                if let Some(l) = g.make_layout(&wstr(sign), 0, false, false) {
                    g.draw_layout(&l, tx, y, sign_c);
                }
            }
            let (text, color) = match d.kind {
                Kind::Hunk => (d.text.as_str(), BLUE.mix(TEXT_DIM, 0.4)),
                Kind::Note => (d.text.as_str(), TEXT_DIM),
                _ => (d.text.as_str(), FG),
            };
            if !text.is_empty() {
                // Only what can be seen: very long lines are cut at the right edge.
                let max_chars = ((text_area.right() - tx) / cw).ceil() as usize + 2;
                let shown: String = text.chars().take(max_chars).collect();
                let fmt = if d.kind == Kind::Note { 2 } else { 0 };
                if let Some(l) = g.make_layout(&wstr(&shown), fmt, false, false) {
                    g.draw_layout(&l, tx + 2.0 * cw, y, color);
                }
            }
        }
        g.unclip();
        // Where we are in the file: a thin bar on the right.
        let total = v.lines.len().max(1) as f32;
        if v.lines.len() > v.page {
            let track = Rect::new(body.right() - 7.0 * s, text_area.y, 4.0 * s, text_area.h);
            let th = (track.h * v.page as f32 / total).max(16.0 * s);
            let ty = track.y + (track.h - th) * (v.top as f32 / (total - v.page as f32).max(1.0));
            g.round(Rect::new(track.x, ty, track.w, th), 2.0 * s, TEXT_DIM, 0.5);
        }
        self.ui.diff = Some(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_unified_diff() {
        let d = "diff --git a/x.rs b/x.rs\nindex 1..2 100644\n--- a/x.rs\n+++ b/x.rs\n@@ -10,3 +10,4 @@ fn main() {\n     let a = 1;\n-    let b = 2;\n+    let b = 3;\n+    let c = 4;\n }\n\\ No newline at end of file\n";
        let l = parse_unified(d);
        let short: Vec<(Kind, Option<u32>, Option<u32>, &str)> = l.iter().map(|x| (x.kind, x.old, x.new, x.text.as_str())).collect();
        assert_eq!(
            short,
            vec![
                (Kind::Hunk, None, None, "@@ -10,3 +10,4 @@ fn main() {"),
                (Kind::Same, Some(10), Some(10), "    let a = 1;"),
                (Kind::Removed, Some(11), None, "    let b = 2;"),
                (Kind::Added, None, Some(11), "    let b = 3;"),
                (Kind::Added, None, Some(12), "    let c = 4;"),
                (Kind::Same, Some(12), Some(13), "}"),
                (Kind::Note, None, None, "No newline at end of file"),
            ]
        );
    }

    #[test]
    fn new_files_and_binary_files() {
        let l = new_file_lines("a\n\tb\n");
        assert_eq!((l[1].kind, l[1].new, l[1].text.as_str()), (Kind::Added, Some(2), "    b"));
        assert_eq!(new_file_lines("PNG\0\0")[0].kind, Kind::Note);
        assert_eq!(parse_unified("diff --git a/p.png b/p.png\nBinary files a/p.png and b/p.png differ\n")[0].kind, Kind::Note);
    }
}
