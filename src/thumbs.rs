//! Small pictures at the right edge of a Claude tab, next to the lines that show them
//! (`[Image #N]` or an image file path). Click one to open it big in a popup over the
//! window; ←/→ move through the other pictures of the same album; Esc or a click closes it.
//!
//! The pictures float over the pane's right edge. The terminal keeps its full width, so
//! a screen without pictures loses nothing. A card is only drawn where the rows under it
//! are empty at those columns (horizontal rule lines count as empty), so it never covers
//! text. If there is no empty spot near its lines, the card is not drawn; hovering the
//! text still shows the picture.
//!
//! Pictures on lines close together (a list of paths, several pastes in one message)
//! become one album card, like a messaging app does with photos sent together:
//! 2 side by side, 3 as one wide + two small, 4 or more as a 2×2 grid where the last
//! tile says "+N" for the ones not shown.
//!
//! Which rows have pictures is worked out once per row content (cached by the row's
//! hash), so drawing stays fast.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct2D::ID2D1Bitmap;

use crate::app::App;
use crate::gfx::{PicSource, Rgb};
use crate::images::{self, ChatImages};
use crate::layout::Rect;
use crate::links;
use crate::pane::Pane;
use crate::preview::Preview;
use crate::theme::*;

/// Thumbnail height in text rows. About 1.5x the size Claude's desktop app uses.
const THUMB_ROWS: f32 = 2.8;
/// Thumbnails are a little wider than tall.
const THUMB_ASPECT: f32 = 1.3;
/// A picture that could not be loaded is tried again after this long.
const RETRY: Duration = Duration::from_secs(3);
/// Pictures whose lines are at most this many rows apart join one album
/// (2 = one empty line between them is still the same album).
const GROUP_GAP: usize = 2;
/// An album shows at most this many tiles. The last one says "+N" for the rest.
const ALBUM_MAX: usize = 4;
/// How many rows up or down a card may move from its lines to find empty space.
const MAX_SHIFT: i32 = 6;

#[derive(Clone, Debug, PartialEq)]
pub enum ThumbRef {
    Tag(u32),
    File(String),
}

/// What one screen row means for thumbnails (cached by the row's content).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowPics {
    /// The pictures this row points to.
    pub refs: Vec<ThumbRef>,
    /// The column just after the row's last visible character. A card may cover
    /// columns from here to the right edge.
    pub end: usize,
}

/// The rows on screen, for drawing the thumbnails of one pane.
pub struct ScreenPics {
    pub cols: usize,
    pub rows: Vec<RowPics>,
}

/// A thumbnail drawn in the last frame, for clicks.
#[derive(Clone, Debug)]
pub struct ThumbHit {
    pub rect: Rect,
    pub pv: Preview,
    /// Index into `ui.albums`: the pictures ←/→ move through in the big view.
    pub album: usize,
}

/// Characters a card may cover: blanks, and the long horizontal lines Claude draws
/// around its input box (covering the end of such a line hides nothing).
pub fn is_free(c: char) -> bool {
    matches!(c, ' ' | '\0' | '\t' | '─' | '━' | '┄' | '┅' | '┈' | '┉' | '╌' | '╍' | '═')
}

/// The pictures one screen row points to.
pub fn scan_row(text: &str, cwd: Option<&str>) -> Vec<ThumbRef> {
    let mut out = Vec::new();
    if text.contains("[Image #") {
        let mut col = 0;
        while let Some(i) = text[col..].find("[Image #") {
            let at = text[..col + i].chars().count();
            if let Some((n, _, _)) = images::tag_at(text, at) {
                out.push(ThumbRef::Tag(n));
            }
            col += i + 1;
        }
    }
    let lower = text.to_ascii_lowercase();
    if images::IMAGE_EXTS.iter().any(|e| lower.contains(&format!(".{e}"))) {
        out.extend(links::image_paths_in(text, cwd).into_iter().map(ThumbRef::File));
    }
    out
}

/// Pictures on nearby rows, shown as one card.
#[derive(Debug, PartialEq)]
struct Group {
    first: usize,
    last: usize,
    pics: Vec<ThumbRef>,
}

/// Puts pictures whose rows are close together into albums, in screen order.
/// A picture seen twice in one album (a wrapped line, a path repeated) is kept once.
fn group_rows(rows: &[RowPics]) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for (row, info) in rows.iter().enumerate() {
        for r in &info.refs {
            match out.last_mut() {
                Some(g) if row <= g.last + GROUP_GAP => {
                    if !g.pics.contains(r) {
                        g.pics.push(r.clone());
                    }
                    g.last = row;
                }
                _ => out.push(Group { first: row, last: row, pics: vec![r.clone()] }),
            }
        }
    }
    out
}

/// Where the tiles of an album go, from its top-left corner. `n` = tiles shown (1..=4).
/// Returns the album's width, height and the tiles.
fn album_tiles(n: usize, tw: f32, th: f32, gap: f32) -> (f32, f32, Vec<Rect>) {
    let w2 = 2.0 * tw + gap;
    let r2 = th + gap;
    match n {
        0 | 1 => (tw, th, vec![Rect::new(0.0, 0.0, tw, th)]),
        2 => (w2, th, vec![Rect::new(0.0, 0.0, tw, th), Rect::new(tw + gap, 0.0, tw, th)]),
        3 => (
            w2,
            r2 + th,
            vec![Rect::new(0.0, 0.0, w2, th), Rect::new(0.0, r2, tw, th), Rect::new(tw + gap, r2, tw, th)],
        ),
        _ => (
            w2,
            r2 + th,
            vec![Rect::new(0.0, 0.0, tw, th), Rect::new(tw + gap, 0.0, tw, th), Rect::new(0.0, r2, tw, th), Rect::new(tw + gap, r2, tw, th)],
        ),
    }
}

/// Row offsets to try, nearest first: 0, -1, +1, -2, +2, ...
fn shifts() -> impl Iterator<Item = i32> {
    (0..=2 * MAX_SHIFT).map(|k| if k % 2 == 1 { -(k + 1) / 2 } else { k / 2 })
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

/// Where a picture comes from: a file, Claude's chat log, or our copy of a paste not sent yet.
pub fn load_source(chats: &mut HashMap<String, ChatImages>, panes: &HashMap<u64, Pane>, pv: &Preview) -> Option<PicSource> {
    if let Some(path) = &pv.path {
        return Some(PicSource::File(path.clone()));
    }
    if let Some(tp) = images::transcript_path(&pv.session) {
        let idx = chats.entry(pv.session.clone()).or_insert_with(|| ChatImages::new(tp));
        idx.update();
        if let Some(b) = idx.bytes(pv.n) {
            return Some(PicSource::Bytes(b));
        }
    }
    panes.get(&pv.pane)?.pending_images.get(&pv.n).cloned().map(PicSource::Bytes)
}

pub fn preview_for(r: &ThumbRef, pane: u64, session: &str) -> Option<Preview> {
    match r {
        ThumbRef::Tag(n) => {
            if session.is_empty() {
                return None;
            }
            Some(Preview { key: format!("img:{session}:{n}"), title: format!("Image #{n}"), pane, session: session.to_string(), n: *n, path: None })
        }
        ThumbRef::File(path) => {
            let name = Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
            Some(Preview { key: format!("file:{path}"), title: name, pane, session: String::new(), n: 0, path: Some(path.clone()) })
        }
    }
}

impl App {
    fn thumb_size(&self) -> (f32, f32) {
        let h = (self.gfx.cell_h * THUMB_ROWS).round();
        ((h * THUMB_ASPECT).round(), h)
    }

    /// A picture, made once and kept. Pictures that fail are not retried for a few seconds.
    fn thumb_picture(&mut self, key: &str, max: u32, pv: &Preview) -> Option<(ID2D1Bitmap, u32, u32)> {
        if self.thumb_miss.get(key).is_some_and(|t| t.elapsed() < RETRY) {
            return None;
        }
        let chats = &mut self.chat_images;
        let panes = &self.panes;
        let got = self.gfx.picture(key, max, || load_source(chats, panes, pv));
        if got.is_none() {
            if self.thumb_miss.len() > 500 {
                self.thumb_miss.clear();
            }
            self.thumb_miss.insert(key.to_string(), Instant::now());
        }
        got
    }

    /// Draw the thumbnails of one pane over its right edge. `area`: the pane's terminal
    /// rectangle; (`ox`, `oy`): where its text starts.
    pub fn paint_thumbs(&mut self, id: u64, area: Rect, ox: f32, oy: f32, screen: ScreenPics) {
        let groups = group_rows(&screen.rows);
        if groups.is_empty() {
            return;
        }
        let session = self.panes.get(&id).and_then(|p| p.stats.claude.as_ref()).map(|c| c.session_id.clone()).unwrap_or_default();
        let (cw, ch) = (self.gfx.cell_w, self.gfx.cell_h);
        let (tw, th) = self.thumb_size();
        let gap = self.s(4.0);
        let tile_gap = self.s(3.0);
        let pad = self.s(3.0);
        let rad = self.s(4.0);
        let max_px = (tw.max(th) * 2.0) as u32;
        let (mx, my) = self.ui.mouse;
        // Right edge of the cards: the last text column, clear of the scroll bar.
        let right = (ox + screen.cols as f32 * cw).min(area.right() - self.s(14.0));
        let jumps: Vec<Rect> = self.ui.jump.iter().filter(|(j, _)| *j == id).map(|(_, r)| *r).collect();
        let mut prev_bottom = area.y;
        for g in groups {
            // Load only the pictures that get a tile (a list of 100 paths loads 4).
            let all: Vec<Preview> = g.pics.iter().filter_map(|r| preview_for(r, id, &session)).collect();
            let mut shown: Vec<(Preview, ID2D1Bitmap, u32, u32)> = Vec::new();
            let mut tried = 0;
            for pv in &all {
                if shown.len() == ALBUM_MAX {
                    break;
                }
                tried += 1;
                if let Some((bmp, w, h)) = self.thumb_picture(&format!("thumb:{}", pv.key), max_px, pv) {
                    shown.push((pv.clone(), bmp, w, h));
                }
            }
            if shown.is_empty() {
                continue;
            }
            // Pictures not shown (not tried yet): the last tile says "+N" for them and itself.
            let rest = all.len() - tried;
            let n = shown.len();
            let (tiles, cw_, chh) = if n == 1 {
                (vec![Rect::new(0.0, 0.0, tw, th)], tw, th)
            } else {
                let (w, h, t) = album_tiles(n, tw, th, tile_gap);
                (t.into_iter().map(|r| Rect::new(r.x + pad, r.y + pad, r.w, r.h)).collect(), w + 2.0 * pad, h + 2.0 * pad)
            };
            let x = (right - cw_).round();
            // First column the card covers, plus one column of air on its left.
            let c0 = ((x - ox) / cw).floor() as isize - 1;
            if c0 < 0 {
                continue;
            }
            let c0 = c0 as usize;
            // Centred on its lines; moved up or down a few rows if something is in the way.
            let mid = (g.first + g.last + 1) as f32 / 2.0;
            let ideal = oy + mid * ch - chh / 2.0;
            let fits = |y: f32| -> bool {
                if y < prev_bottom + gap || y + chh > area.bottom() - gap {
                    return false;
                }
                let card = Rect::new(x, y, cw_, chh);
                if jumps.iter().any(|j| overlaps(j, &card)) {
                    return false;
                }
                let r0 = ((y - oy) / ch).floor().max(0.0) as usize;
                let r1 = ((y + chh - 1.0 - oy) / ch).floor().max(0.0) as usize;
                (r0..=r1).all(|row| screen.rows.get(row).is_none_or(|info| info.end <= c0))
            };
            let Some(y) = shifts().map(|d| (ideal + d as f32 * ch).round()).find(|y| fits(*y)) else { continue };
            let card = Rect::new(x, y, cw_, chh);
            prev_bottom = card.bottom();

            let album = self.ui.albums.len();
            self.ui.albums.push(all);
            let g = &self.gfx;
            g.round(card, rad, CARD, 1.0);
            for (i, ((pv, bmp, w, h), t)) in shown.into_iter().zip(tiles).enumerate() {
                let tile = Rect::new(x + t.x, y + t.y, t.w, t.h);
                if n == 1 {
                    // One picture: show all of it (no cropping), centred.
                    let inner = tile.inset(self.s(2.0));
                    let k = (inner.w / w as f32).min(inner.h / h as f32);
                    let (pw, ph) = ((w as f32 * k).round().max(1.0), (h as f32 * k).round().max(1.0));
                    g.draw_picture(&bmp, Rect::new((inner.x + (inner.w - pw) / 2.0).round(), (inner.y + (inner.h - ph) / 2.0).round(), pw, ph));
                } else {
                    // Album tiles are filled edge to edge (the middle of the picture), like a photo grid.
                    g.draw_picture_cover(&bmp, tile);
                }
                if i == ALBUM_MAX - 1 && rest > 0 {
                    g.fill_a(tile, Rgb::hex(0x000000), 0.55);
                    g.text(&format!("+{}", rest + 1), &g.ui_center, tile, TEXT);
                }
                if n > 1 && tile.contains(mx, my) {
                    g.outline(tile, 0.0, ACCENT, 1.5);
                }
                self.ui.thumbs.push(ThumbHit { rect: tile, pv, album });
            }
            let hot = n == 1 && card.contains(mx, my);
            g.outline(card, rad, if hot { ACCENT } else { BORDER }, if hot { 1.5 } else { 1.0 });
        }
    }

    pub fn thumb_hit(&self, x: f32, y: f32) -> Option<ThumbHit> {
        self.ui.thumbs.iter().find(|t| t.rect.contains(x, y)).cloned()
    }

    pub fn thumb_at(&self, x: f32, y: f32) -> Option<Preview> {
        self.thumb_hit(x, y).map(|t| t.pv)
    }

    /// Opens a picture big. `album`: the pictures ←/→ move through (empty for none).
    pub fn open_modal(&mut self, pv: Preview, album: Vec<Preview>) {
        self.ui.preview = None;
        self.ui.modal = Some(pv);
        self.ui.modal_album = album;
        self.invalidate();
    }

    /// ←/→ in the big view: the previous / next picture of its album (wraps around).
    /// Returns false when there is no album to move through.
    pub fn modal_step(&mut self, step: i32) -> bool {
        let Some(cur) = &self.ui.modal else { return false };
        let album = &self.ui.modal_album;
        if album.len() < 2 {
            return false;
        }
        let i = album.iter().position(|p| p.key == cur.key).unwrap_or(0) as i32;
        let n = album.len() as i32;
        self.ui.modal = Some(album[(i + step).rem_euclid(n) as usize].clone());
        self.invalidate();
        true
    }

    pub fn close_modal(&mut self) -> bool {
        self.ui.modal_album.clear();
        if self.ui.modal.take().is_some() {
            self.invalidate();
            return true;
        }
        false
    }

    /// The big picture over everything, with the window dimmed behind it.
    pub fn paint_modal(&mut self) {
        let Some(pv) = self.ui.modal.clone() else { return };
        let (w, h) = self.client();
        let s = self.gfx.scale;
        self.gfx.fill_a(Rect::new(0.0, 0.0, w, h), Rgb::hex(0x000000), 0.72);
        let pic = self.thumb_picture(&format!("big:{}", pv.key), w.max(h) as u32, &pv);
        // "3 / 7" when it is part of an album.
        let count = match self.ui.modal_album.len() {
            n if n > 1 => {
                let i = self.ui.modal_album.iter().position(|p| p.key == pv.key).unwrap_or(0);
                format!("  ·  {} / {n}  ·  ← → for the others", i + 1)
            }
            _ => String::new(),
        };
        let g = &self.gfx;
        let pad = (10.0 * s).round();
        let cap_h = (24.0 * s).round();
        let (caption, pw, ph) = match &pic {
            Some((_, iw, ih)) => {
                // As big as fits (90% of the window), never larger than the picture itself.
                let k = ((w * 0.9 - 2.0 * pad) / *iw as f32).min((h * 0.88 - 2.0 * pad - cap_h) / *ih as f32).min(1.0);
                (
                    format!("{}  ·  {} × {}{count}  ·  Esc or click to close", pv.title, iw, ih),
                    (*iw as f32 * k).round().max(1.0),
                    (*ih as f32 * k).round().max(1.0),
                )
            }
            None => (format!("{}: picture not found{count}  ·  Esc to close", pv.title), self.s(320.0), 0.0),
        };
        let cw = (pw + 2.0 * pad).max(g.measure(&caption, &g.ui) + 2.0 * pad);
        let chh = ph + 2.0 * pad + cap_h;
        let card = Rect::new(((w - cw) / 2.0).round(), ((h - chh) / 2.0).round(), cw.round(), chh.round());
        g.round(card, 10.0 * s, CARD, 1.0);
        g.outline(card, 10.0 * s, BORDER, 1.0);
        if let Some((bmp, _, _)) = &pic {
            g.draw_picture(bmp, Rect::new(card.x + ((card.w - pw) / 2.0).round(), card.y + pad, pw, ph));
        }
        g.text(&caption, &g.ui, Rect::new(card.x + pad, card.bottom() - cap_h - pad / 2.0, card.w - 2.0 * pad, cap_h), TEXT_DIM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(refs: &[ThumbRef]) -> RowPics {
        RowPics { refs: refs.to_vec(), end: 10 }
    }

    fn file(s: &str) -> ThumbRef {
        ThumbRef::File(s.into())
    }

    #[test]
    fn finds_tags_in_a_row() {
        let r = scan_row("❯ look at [Image #3] and [Image #12] please", None);
        assert_eq!(r, vec![ThumbRef::Tag(3), ThumbRef::Tag(12)]);
        assert!(scan_row("no pictures here", None).is_empty());
    }

    #[test]
    fn nearby_rows_become_one_album() {
        // Rows 0-4: five paths in a list. Row 5-6 empty. Row 9: a picture far below.
        let mut rows = vec![RowPics::default(); 12];
        for (i, name) in ["a.png", "b.png", "c.png", "d.png", "e.png"].iter().enumerate() {
            rows[i] = row(&[file(name)]);
        }
        rows[9] = row(&[ThumbRef::Tag(1)]);
        let g = group_rows(&rows);
        assert_eq!(g.len(), 2);
        assert_eq!((g[0].first, g[0].last, g[0].pics.len()), (0, 4, 5));
        assert_eq!((g[1].first, g[1].last, &g[1].pics), (9, 9, &vec![ThumbRef::Tag(1)]));
    }

    #[test]
    fn one_empty_line_keeps_the_album_and_repeats_count_once() {
        let mut rows = vec![RowPics::default(); 6];
        rows[0] = row(&[file("a.png")]);
        rows[1] = row(&[file("a.png")]); // the same path wrapped onto the next row
        rows[3] = row(&[file("b.png"), ThumbRef::Tag(2)]); // one empty row between
        let g = group_rows(&rows);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].pics, vec![file("a.png"), file("b.png"), ThumbRef::Tag(2)]);
    }

    #[test]
    fn album_layouts() {
        let (w, h, t) = album_tiles(1, 60.0, 40.0, 2.0);
        assert_eq!((w, h, t.len()), (60.0, 40.0, 1));
        let (w, h, t) = album_tiles(2, 60.0, 40.0, 2.0);
        assert_eq!((w, h, t.len()), (122.0, 40.0, 2));
        // Three: one wide on top, two below.
        let (w, h, t) = album_tiles(3, 60.0, 40.0, 2.0);
        assert_eq!((w, h), (122.0, 82.0));
        assert_eq!(t[0].w, 122.0);
        assert_eq!((t[2].x, t[2].y), (62.0, 42.0));
        // Four or more: 2×2.
        let (w, h, t) = album_tiles(4, 60.0, 40.0, 2.0);
        assert_eq!((w, h, t.len()), (122.0, 82.0, 4));
        assert_eq!((t[3].x, t[3].y), (62.0, 42.0));
    }

    #[test]
    fn shifts_go_nearest_first() {
        let v: Vec<i32> = shifts().take(5).collect();
        assert_eq!(v, vec![0, -1, 1, -2, 2]);
    }

    #[test]
    fn rule_lines_count_as_empty() {
        assert!(is_free(' ') && is_free('─'));
        assert!(!is_free('a') && !is_free('│') && !is_free('⏺'));
    }
}
