//! Small pictures at the right edge of a Claude tab, next to each line that shows one
//! (`[Image #N]` or an image file path). Click one to open it big in a popup over the
//! window; Esc or a click closes it.
//!
//! The pane gives up a narrow strip on its right for them (a few columns), so they
//! never cover text. Which rows have pictures is worked out once per row content
//! (cached by the row's hash), so drawing stays fast.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

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

#[derive(Clone, Debug, PartialEq)]
pub enum ThumbRef {
    Tag(u32),
    File(String),
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

    /// Width of the strip a Claude pane gives up for thumbnails.
    pub fn thumb_gutter_w(&self) -> f32 {
        (self.thumb_size().0 + self.s(16.0)).round()
    }

    /// A picture, made once and kept. Pictures that fail are not retried for a few seconds.
    fn thumb_picture(&mut self, key: &str, max: u32, pv: &Preview) -> Option<(windows::Win32::Graphics::Direct2D::ID2D1Bitmap, u32, u32)> {
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

    /// Draw the thumbnails of one pane. `rows`: (screen row, picture) in screen order.
    pub fn paint_thumbs(&mut self, id: u64, gutter: Rect, origin_y: f32, rows: Vec<(usize, ThumbRef)>) {
        if rows.is_empty() {
            return;
        }
        let session = self.panes.get(&id).and_then(|p| p.stats.claude.as_ref()).map(|c| c.session_id.clone()).unwrap_or_default();
        let ch = self.gfx.cell_h;
        let (tw, th) = self.thumb_size();
        let gap = self.s(4.0);
        let x = (gutter.x + (gutter.w - tw) / 2.0).round();
        let max_px = (tw.max(th) * 2.0) as u32;
        let (mx, my) = self.ui.mouse;
        let mut prev_bottom = f32::MIN;
        let mut last: Option<ThumbRef> = None;
        for (row, r) in rows {
            // The same picture on the next row (a wrapped line): one thumbnail is enough.
            if last.as_ref() == Some(&r) {
                continue;
            }
            last = Some(r.clone());
            let Some(pv) = preview_for(&r, id, &session) else { continue };
            // Centred on its row; pushed down if it would touch the one above.
            let mut y = (origin_y + row as f32 * ch + ch / 2.0 - th / 2.0).round();
            if y < prev_bottom + gap {
                y = prev_bottom + gap;
            }
            y = y.max(gutter.y + gap);
            if y + th > gutter.bottom() {
                break;
            }
            let Some((bmp, w, h)) = self.thumb_picture(&format!("thumb:{}", pv.key), max_px, &pv) else { continue };
            let rect = Rect::new(x, y, tw, th);
            prev_bottom = rect.bottom();
            let g = &self.gfx;
            let rad = self.s(4.0);
            g.round(rect, rad, CARD, 1.0);
            // Fit the whole picture inside (no cropping), centred.
            let inner = rect.inset(self.s(2.0));
            let k = (inner.w / w as f32).min(inner.h / h as f32);
            let (pw, ph) = ((w as f32 * k).round().max(1.0), (h as f32 * k).round().max(1.0));
            g.draw_picture(&bmp, Rect::new((inner.x + (inner.w - pw) / 2.0).round(), (inner.y + (inner.h - ph) / 2.0).round(), pw, ph));
            let hot = rect.contains(mx, my);
            g.outline(rect, rad, if hot { ACCENT } else { BORDER }, if hot { 1.5 } else { 1.0 });
            self.ui.thumbs.push((rect, pv));
        }
    }

    pub fn thumb_at(&self, x: f32, y: f32) -> Option<Preview> {
        self.ui.thumbs.iter().find(|(r, _)| r.contains(x, y)).map(|(_, p)| p.clone())
    }

    pub fn open_modal(&mut self, pv: Preview) {
        self.ui.preview = None;
        self.ui.modal = Some(pv);
        self.invalidate();
    }

    pub fn close_modal(&mut self) -> bool {
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
        let g = &self.gfx;
        let pad = (10.0 * s).round();
        let cap_h = (24.0 * s).round();
        let (caption, pw, ph) = match &pic {
            Some((_, iw, ih)) => {
                // As big as fits (90% of the window), never larger than the picture itself.
                let k = ((w * 0.9 - 2.0 * pad) / *iw as f32).min((h * 0.88 - 2.0 * pad - cap_h) / *ih as f32).min(1.0);
                (
                    format!("{}  ·  {} × {}  ·  Esc or click to close", pv.title, iw, ih),
                    (*iw as f32 * k).round().max(1.0),
                    (*ih as f32 * k).round().max(1.0),
                )
            }
            None => (format!("{}: picture not found  ·  Esc to close", pv.title), self.s(320.0), 0.0),
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

    #[test]
    fn finds_tags_in_a_row() {
        let r = scan_row("❯ look at [Image #3] and [Image #12] please", None);
        assert_eq!(r, vec![ThumbRef::Tag(3), ThumbRef::Tag(12)]);
        assert!(scan_row("no pictures here", None).is_empty());
    }
}
