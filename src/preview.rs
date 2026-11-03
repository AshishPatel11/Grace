//! Picture previews: hover `[Image #N]` or an image file path and the picture pops up.
//! Where the pictures come from is explained in `images.rs`.

use std::path::Path;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;

use crate::app::{App, Drag};
use crate::gfx::PicSource;
use crate::images::{self, ChatImages};
use crate::layout::Rect;
use crate::links::{self, Target};
use crate::theme::*;

/// The picture under the mouse.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    /// Cache key: "img:<chat id>:<n>" or "file:<path>".
    pub key: String,
    pub title: String,
    pub pane: u64,
    pub session: String,
    /// The N of `[Image #N]` (0 for a file).
    pub n: u32,
    pub path: Option<String>,
}

impl App {
    /// Is the mouse over a picture reference? Called on mouse moves.
    pub fn preview_at(&self, x: f32, y: f32) -> Option<Preview> {
        if !matches!(self.ui.drag, Drag::None) {
            return None;
        }
        let (id, r) = self.pane_at(x, y)?;
        let (pt, _) = self.grid_point(id, r, x, y)?;
        let p = self.panes.get(&id)?;
        let l = p.live.as_ref()?;
        let session = p.stats.claude.as_ref().map(|c| c.session_id.clone()).unwrap_or_default();
        let t = l.term.lock();
        if pt.line < t.grid().topmost_line() || pt.line > t.grid().bottommost_line() {
            return None;
        }
        // One character per screen column, so a column is also a character index.
        let row = &t.grid()[pt.line];
        let text: String = (0..t.columns()).map(|c| row[Column(c)].c).collect();
        if let Some((n, _, _)) = images::tag_at(&text, pt.column.0) {
            if session.is_empty() {
                return None;
            }
            return Some(Preview { key: format!("img:{session}:{n}"), title: format!("Image #{n}"), pane: id, session, n, path: None });
        }
        if let Some(link) = links::find_at(&*t, pt, p.cwd.as_deref()) {
            if let Target::File { path, .. } = link.target {
                if images::is_image_path(&path) {
                    let name = Path::new(&path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
                    return Some(Preview { key: format!("file:{path}"), title: name, pane: id, session: String::new(), n: 0, path: Some(path) });
                }
            }
        }
        None
    }

    /// A picture was just pasted into Claude (Alt+V). Keep a copy, because Claude only
    /// saves it in the chat log once the message is sent.
    pub fn note_pasted_image(&mut self, id: u64) {
        let Some(bytes) = images::clipboard_image(self.hwnd) else { return };
        let Some(p) = self.panes.get(&id) else { return };
        let session = p.stats.claude.as_ref().map(|c| c.session_id.clone()).unwrap_or_default();
        // Claude numbers pictures across the whole chat, so the next one is one past the highest.
        let mut base = 0;
        if let Some(path) = images::transcript_path(&session) {
            let idx = self.chat_images.entry(session.clone()).or_insert_with(|| ChatImages::new(path));
            idx.update();
            base = idx.max_n;
        }
        let p = self.panes.get_mut(&id).unwrap();
        p.pending_images.retain(|n, _| *n > base);
        let n = p.pending_images.keys().copied().max().unwrap_or(0).max(base) + 1;
        p.pending_images.insert(n, bytes);
    }

    pub fn paint_preview(&mut self) {
        let Some(pv) = self.ui.preview.clone() else { return };
        let s = self.gfx.scale;
        let max_px = (560.0 * s) as u32;
        let chats = &mut self.chat_images;
        let panes = &self.panes;
        let pic = self.gfx.picture(&pv.key, max_px, || {
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
            // Pasted but not sent yet: our own copy.
            panes.get(&pv.pane)?.pending_images.get(&pv.n).cloned().map(PicSource::Bytes)
        });
        let g = &self.gfx;
        let (cw, chh) = self.client();
        let (mx, my) = self.ui.mouse;
        let pad = (8.0 * s).round();
        let cap_h = (22.0 * s).round();
        let (caption, pw, ph) = match &pic {
            Some((_, w, h)) => {
                // Fit inside 520 x 380 (and 60% of the window), never bigger than the picture.
                let maxw = (520.0 * s).min(cw * 0.6);
                let maxh = (380.0 * s).min(chh * 0.6);
                let k = (maxw / *w as f32).min(maxh / *h as f32).min(1.0);
                (format!("{}  ·  {} × {}", pv.title, w, h), (*w as f32 * k).round().max(1.0), (*h as f32 * k).round().max(1.0))
            }
            None => (format!("{}: picture not found", pv.title), 0.0, 0.0),
        };
        let text_w = g.measure(&caption, &g.ui) + 2.0 * pad;
        let bw = (pw + 2.0 * pad).max(text_w);
        let bh = if pic.is_some() { ph + 2.0 * pad + cap_h } else { cap_h + pad };
        // Below-right of the mouse; flip to the other side when it would not fit.
        let mut x = mx + 16.0 * s;
        let mut y = my + 18.0 * s;
        if x + bw > cw - 6.0 {
            x = (mx - 16.0 * s - bw).max(6.0);
        }
        if y + bh > chh - 6.0 {
            y = (my - 12.0 * s - bh).max(self.bar_h() + 4.0);
        }
        let card = Rect::new(x.round(), y.round(), bw.round(), bh.round());
        g.round(card, 8.0 * s, CARD, 0.99);
        g.outline(card, 8.0 * s, BORDER, 1.0);
        if let Some((bmp, _, _)) = &pic {
            let img = Rect::new(card.x + ((card.w - pw) / 2.0).round(), card.y + pad, pw, ph);
            g.draw_picture(bmp, img);
            g.text(&caption, &g.ui, Rect::new(card.x + pad, img.bottom() + 2.0 * s, card.w - 2.0 * pad, cap_h), TEXT_DIM);
        } else {
            g.text(&caption, &g.ui, Rect::new(card.x + pad, card.y + pad / 2.0, card.w - 2.0 * pad, cap_h), TEXT_DIM);
        }
    }
}
