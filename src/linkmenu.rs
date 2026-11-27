//! The menu that opens when you click a link: Grace's own dropdown (not Windows' plain menu).
//!
//! A card at the mouse with the link on top (icon, name, where it points), then the actions
//! with icons: Open, Open in VS Code, Copy, Show in File Explorer. Mouse or ↑↓ Enter; Esc,
//! a click outside or the wheel closes it. The actions themselves are in `App::link_command`.

use crate::app::App;
use crate::gfx::Rgb;
use crate::layout::Rect;
use crate::links::Target;
use crate::theme::*;

/// Icons from Windows' icon font (Segoe Fluent Icons / Segoe MDL2 Assets).
pub const I_OPEN: char = '\u{E8A7}';
pub const I_CODE: char = '\u{E943}';
pub const I_COPY: char = '\u{E8C8}';
pub const I_FOLDER: char = '\u{E8B7}';
pub const I_EXPLORER: char = '\u{EC50}';
const I_GLOBE: char = '\u{E774}';
const I_DOC: char = '\u{E8A5}';
const I_PHOTO: char = '\u{EB9F}';
const I_VIDEO: char = '\u{E714}';
const I_MUSIC: char = '\u{E8D6}';

/// The card and its rows (`MENU_BG` is a little lighter than the tabs so it stands out).
const MENU_BG: Rgb = Rgb::hex(0x1F232B);
const MENU_EDGE: Rgb = Rgb::hex(0x343A46);

pub struct Item {
    /// What it does (`App::link_command`); 0 = a dividing line.
    pub cmd: u32,
    pub icon: char,
    pub label: String,
    /// Shown on the right as a small key cap ("Ctrl+Click").
    pub hint: String,
}

impl Item {
    pub fn new(cmd: u32, icon: char, label: impl Into<String>) -> Item {
        Item { cmd, icon, label: label.into(), hint: String::new() }
    }
    pub fn hint(mut self, h: &str) -> Item {
        self.hint = h.into();
        self
    }
    pub fn line() -> Item {
        Item::new(0, ' ', "")
    }
}

pub struct LinkMenu {
    pub target: Target,
    pub items: Vec<Item>,
    /// Where the click was (window pixels); the card opens there.
    pub at: (f32, f32),
    /// The row under the mouse or picked with ↑↓ (an index into `items`).
    pub sel: Option<usize>,
    /// Where it was drawn last (for clicks).
    pub card: Rect,
    pub rows: Vec<(Rect, usize)>,
}

/// The icon, name and second line for the top of the card.
fn header(t: &Target) -> (char, String, String) {
    match t {
        Target::Url(u) => {
            let rest = u.split_once("://").map_or(u.as_str(), |(_, r)| r);
            let host = rest.split(['/', '?', '#']).next().unwrap_or(rest).trim_start_matches("www.");
            (I_GLOBE, host.to_string(), u.clone())
        }
        Target::File { path, line, .. } => {
            let p = std::path::Path::new(path);
            let name = p.file_name().map_or(path.clone(), |n| n.to_string_lossy().into_owned());
            let name = match line {
                Some(l) => format!("{name}  ·  line {l}"),
                None => name,
            };
            let folder = p.parent().map_or(String::new(), |d| d.to_string_lossy().into_owned());
            let ext = crate::links::ext_of(path).unwrap_or_default().to_ascii_lowercase();
            let icon = if p.is_dir() {
                I_FOLDER
            } else if crate::images::is_image_path(path) || matches!(ext.as_str(), ".ico" | ".svg" | ".heic") {
                I_PHOTO
            } else if matches!(ext.as_str(), ".mp4" | ".mkv" | ".mov" | ".avi" | ".webm" | ".wmv" | ".m4v") {
                I_VIDEO
            } else if matches!(ext.as_str(), ".mp3" | ".wav" | ".flac" | ".m4a" | ".ogg" | ".aac") {
                I_MUSIC
            } else if crate::links::is_code(path) {
                I_CODE
            } else {
                I_DOC
            };
            (icon, name, folder)
        }
    }
}

impl App {
    pub fn open_link_menu(&mut self, target: Target, items: Vec<Item>, x: f32, y: f32) {
        self.ui.link_menu = Some(LinkMenu { target, items, at: (x, y), sel: None, card: Rect::default(), rows: Vec::new() });
        self.invalidate();
    }

    fn run_link_item(&mut self, i: usize) {
        let Some(m) = self.ui.link_menu.as_ref() else { return };
        let Some(cmd) = m.items.get(i).map(|it| it.cmd).filter(|c| *c != 0) else { return };
        self.link_command(cmd);
        self.ui.link_menu = None;
        self.invalidate();
    }

    /// Keys while the menu is open. Other keys close it and do what they normally do.
    pub fn link_menu_key(&mut self, vk: u16) -> bool {
        use crate::input::*;
        let Some(m) = &mut self.ui.link_menu else { return false };
        let real: Vec<usize> = (0..m.items.len()).filter(|i| m.items[*i].cmd != 0).collect();
        let pos = m.sel.and_then(|s| real.iter().position(|i| *i == s));
        match vk {
            VK_ESCAPE => self.ui.link_menu = None,
            VK_DOWN => m.sel = Some(real[pos.map_or(0, |p| (p + 1) % real.len())]),
            VK_UP => m.sel = Some(real[pos.map_or(real.len() - 1, |p| (p + real.len() - 1) % real.len())]),
            VK_HOME => m.sel = real.first().copied(),
            VK_END => m.sel = real.last().copied(),
            VK_RETURN => {
                let i = m.sel.unwrap_or(real[0]);
                self.run_link_item(i);
            }
            0x10 | 0x11 | 0x12 => return true, // Shift, Ctrl, Alt alone: keep it open
            _ => {
                self.ui.link_menu = None;
                self.invalidate();
                return false;
            }
        }
        self.invalidate();
        true
    }

    /// The mouse moved: the row under it lights up.
    pub fn link_menu_hover(&mut self, x: f32, y: f32) -> bool {
        let Some(m) = &mut self.ui.link_menu else { return false };
        if m.card.contains(x, y) {
            m.sel = m.rows.iter().find(|(r, _)| r.contains(x, y)).map(|(_, i)| *i);
        }
        true
    }

    /// A press while the menu is open: a row runs it, outside closes the menu.
    pub fn link_menu_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let Some(m) = &self.ui.link_menu else { return false };
        if let Some((_, i)) = m.rows.iter().find(|(r, _)| r.contains(x, y)).copied() {
            self.run_link_item(i);
        } else if !m.card.contains(x, y) {
            self.ui.link_menu = None;
            self.invalidate();
        }
        true
    }

    pub fn link_menu_cursor(&self, x: f32, y: f32) -> Option<u16> {
        let m = self.ui.link_menu.as_ref()?;
        Some(if m.rows.iter().any(|(r, _)| r.contains(x, y)) { 2 } else { 0 })
    }

    pub fn paint_link_menu(&mut self) {
        let Some(mut m) = self.ui.link_menu.take() else { return };
        let g = &self.gfx;
        let s = g.scale;
        let (cw, ch) = self.client();
        let (pad, head_h, row_h, line_h) = ((6.0 * s).round(), (54.0 * s).round(), (32.0 * s).round(), (9.0 * s).round());
        let (icon, title, sub) = header(&m.target);

        // Width: the widest row or the header, within limits.
        let hint_w = |h: &str| if h.is_empty() { 0.0 } else { g.measure(h, &g.ui_small) + 12.0 * s };
        let mut w: f32 = 0.0;
        for it in m.items.iter().filter(|it| it.cmd != 0) {
            w = w.max(pad + 40.0 * s + g.measure(&it.label, &g.ui) + 28.0 * s + hint_w(&it.hint) + 10.0 * s + pad);
        }
        w = w.max(62.0 * s + g.measure(&title, &g.ui_bold).max(g.measure(&sub, &g.ui_small)) + 16.0 * s);
        let w = w.clamp(250.0 * s, (460.0 * s).min(cw - 16.0 * s)).round();
        let body: f32 = m.items.iter().map(|it| if it.cmd == 0 { line_h } else { row_h }).sum();
        let h = head_h + 1.0 + 2.0 * pad + body;

        // At the mouse, flipped left / up when it would not fit.
        let (ax, ay) = m.at;
        let mut x = ax + 2.0 * s;
        if x + w > cw - 8.0 * s {
            x = ax - w - 2.0 * s;
        }
        let mut y = ay + 8.0 * s;
        if y + h > ch - 8.0 * s {
            y = ay - h - 8.0 * s;
        }
        let x = x.clamp(8.0 * s, (cw - w - 8.0 * s).max(8.0 * s)).round();
        let y = y.clamp(self.bar_h() + 4.0 * s, (ch - h - 8.0 * s).max(self.bar_h())).round();
        let card = Rect::new(x, y, w, h);
        m.card = card;
        let radius = 10.0 * s;

        // Soft shadow, then the card.
        let black = Rgb::hex(0x000000);
        for (grow, drop, a) in [(14.0, 8.0, 0.10), (8.0, 5.0, 0.14), (3.0, 2.0, 0.22)] {
            let r = card.inset(-grow * s);
            g.round(Rect::new(r.x, r.y + drop * s, r.w, r.h), radius + grow * s, black, a);
        }
        g.round(card, radius, MENU_BG, 1.0);
        g.outline(card, radius, MENU_EDGE, 1.0);

        // The link: icon in a tinted square, its name, and where it points.
        let tile = Rect::new(x + 12.0 * s, y + (head_h - 32.0 * s) / 2.0, 32.0 * s, 32.0 * s);
        g.round(tile, 8.0 * s, LINK_HOVER, 0.14);
        g.text(&icon.to_string(), &g.icons, tile, LINK_HOVER);
        let tx = tile.right() + 12.0 * s;
        let tw = card.right() - tx - 14.0 * s;
        g.text(&title, &g.ui_bold, Rect::new(tx, y + 9.0 * s, tw, 19.0 * s), HEAD_TEXT);
        g.text(&sub, &g.ui_small, Rect::new(tx, y + 28.0 * s, tw, 16.0 * s), TEXT_DIM);
        g.fill(Rect::new(x + 1.0, y + head_h, w - 2.0, 1.0), MENU_EDGE);

        // The actions.
        m.rows.clear();
        let mut ry = y + head_h + 1.0 + pad;
        for (i, it) in m.items.iter().enumerate() {
            if it.cmd == 0 {
                g.fill(Rect::new(x + 14.0 * s, (ry + line_h / 2.0).round(), w - 28.0 * s, 1.0), MENU_EDGE);
                ry += line_h;
                continue;
            }
            let r = Rect::new(x + pad, ry, w - 2.0 * pad, row_h);
            let on = m.sel == Some(i);
            if on {
                g.round(r, 6.0 * s, SELECTION, 0.85);
            }
            g.text(&it.icon.to_string(), &g.icons, Rect::new(r.x + 8.0 * s, r.y, 22.0 * s, r.h), if on { LINK_HOVER } else { TEXT_DIM });
            let hw = hint_w(&it.hint);
            g.text(&it.label, &g.ui, Rect::new(r.x + 40.0 * s, r.y, r.w - 48.0 * s - hw, r.h), if on { HEAD_TEXT } else { TEXT });
            if hw > 0.0 {
                let kh = 18.0 * s;
                let k = Rect::new(r.right() - 8.0 * s - hw, r.y + (r.h - kh) / 2.0, hw, kh);
                g.round(k, 4.0 * s, PILL, 1.0);
                g.text(&it.hint, &g.ui_small_center, k, TEXT_DIM);
            }
            m.rows.push((r, i));
            ry += row_h;
        }
        self.ui.link_menu = Some(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_header() {
        let (i, t, s) = header(&Target::Url("https://www.example.com/docs/page?x=1".into()));
        assert_eq!((i, t.as_str(), s.as_str()), (I_GLOBE, "example.com", "https://www.example.com/docs/page?x=1"));
        let (i, t, s) = header(&Target::File { path: r"C:\a\report.pdf".into(), line: None, col: None });
        assert_eq!((i, t.as_str(), s.as_str()), (I_DOC, "report.pdf", r"C:\a"));
        let (i, t, _) = header(&Target::File { path: r"C:\a\app.rs".into(), line: Some(42), col: None });
        assert_eq!((i, t.as_str()), (I_CODE, "app.rs  ·  line 42"));
        let (i, _, _) = header(&Target::File { path: r"C:\a\clip.mp4".into(), line: None, col: None });
        assert_eq!(i, I_VIDEO);
    }
}
