//! The thin bar under a Claude tab (like Warp's): a mic button and a "Rich input" button.
//!
//! - Mic: listens and types what you say into Claude's own input line (or into the
//!   Rich input box when it is open). It turns red while it listens, and the words heard
//!   so far show next to the buttons. See `voice.rs`.
//! - Rich input: opens or closes the Word-like typing box for this tab (Ctrl+Shift+I).
//! - Folder: shows the folder this tab works in. Click it to open Claude in another
//!   folder (in a new tab), or to show this folder in Explorer. See `folders.rs`.

use windows::Win32::UI::Input::KeyboardAndMouse::SetCapture;

use crate::app::App;
use crate::gfx::Rgb;
use crate::layout::Rect;
use crate::theme::*;

/// Height of the bar (before DPI scaling).
pub const TOOL_H: f32 = 30.0;
const CHIP_H: f32 = 22.0;

#[derive(Clone, Copy, Debug)]
pub struct ToolHit {
    pub id: u64,
    pub rect: Rect,
    pub mic: Rect,
    pub rich: Rect,
    pub folder: Rect,
}

const RICH_LABEL: &str = "Rich input";
const RICH_KEYS: &str = "Ctrl+Shift+I";

impl App {
    /// Should this pane have the bar? (A running Claude.)
    pub fn toolbar_wanted(&self, id: u64) -> bool {
        self.cfg.toolbar && self.panes.get(&id).is_some_and(|p| p.dormant.is_none() && p.is_claude())
    }

    /// The bar at the bottom of pane rect `r`.
    pub fn toolbar_geometry(&self, id: u64, r: Rect) -> ToolHit {
        let g = &self.gfx;
        let h = self.s(TOOL_H).round();
        let rect = Rect::new(r.x, r.bottom() - h, r.w, h);
        let ch = self.s(CHIP_H).round();
        let y = (rect.y + (h - ch) / 2.0).round();
        let x0 = (r.x + self.s(10.0)).round();
        let mic = Rect::new(x0, y, ch, ch);
        let pad = self.s(8.0);
        let w = pad + self.s(14.0) + self.s(6.0) + g.measure(RICH_LABEL, &g.ui_small) + self.s(8.0) + g.measure(RICH_KEYS, &g.ui_small) + pad;
        let rich = Rect::new(mic.right() + self.s(6.0), y, w.round(), ch);
        let name = self.folder_label(id);
        let fw = pad + self.s(14.0) + self.s(6.0) + g.measure(&name, &g.ui_small) + pad;
        let folder = Rect::new(rich.right() + self.s(6.0), y, fw.round().min((r.right() - rich.right() - self.s(16.0)).max(0.0)), ch);
        ToolHit { id, rect, mic, rich, folder }
    }

    /// The folder button's text: the last part of the tab's folder ("Classpulse").
    fn folder_label(&self, id: u64) -> String {
        self.pane_folder(id).map(|d| crate::folders::short_name(&d)).unwrap_or_else(|| "Folder".into())
    }

    pub fn toolbar_at(&self, x: f32, y: f32) -> Option<ToolHit> {
        self.ui.tools.iter().find(|t| t.rect.contains(x, y)).copied()
    }

    /// Mouse press on a bar. Returns true when it was on one.
    pub fn toolbar_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let Some(t) = self.toolbar_at(x, y) else { return false };
        self.focus_pane(t.id);
        if t.mic.contains(x, y) {
            self.guess_clear();
            self.toggle_voice();
        } else if t.rich.contains(x, y) {
            self.box_toggle();
        } else if t.folder.contains(x, y) {
            let (sx, sy) = self.to_screen(t.folder.x, t.folder.bottom());
            self.folder_menu(t.id, sx, sy);
            return true;
        }
        unsafe {
            SetCapture(self.hwnd);
        }
        self.invalidate();
        true
    }

    /// The mic button's colors: (background, icon). Red while it listens.
    pub fn mic_colors(&self, id: u64, hot: bool) -> (Option<Rgb>, Rgb) {
        match self.voice_on(id) {
            Some(v) if v.stopping => (Some(TEXT_DIM.mix(BG, 0.5)), TEXT),
            Some(v) if v.engine.is_none() => (Some(RED.mix(BG, 0.5)), CURSOR),
            Some(_) => (Some(RED), CURSOR),
            None if hot => (Some(TAB_HOVER), TEXT),
            None => (None, TEXT_DIM),
        }
    }

    /// Next to the buttons while the mic listens: "Listening" and the words so far.
    fn paint_voice_status(&self, t: &ToolHit) {
        let Some(v) = self.voice_on(t.id) else { return };
        let g = &self.gfx;
        let after = if t.folder.w > self.s(40.0) { t.folder.right() } else { t.rich.right() };
        let x0 = after + self.s(12.0);
        let right = t.rect.right() - self.s(10.0);
        if right - x0 < self.s(60.0) {
            return;
        }
        let live = v.engine.is_some() && !v.stopping;
        let label = match (v.stopping, v.engine) {
            (true, _) => "Finishing…",
            (false, None) => "Starting the mic…",
            (false, Some(crate::voice::Engine::Modern)) => "Listening",
            (false, Some(crate::voice::Engine::Offline)) => "Listening (offline)",
        };
        let dot = self.s(3.5);
        g.circle(x0 + dot, t.rect.cy(), dot, if live { RED } else { AMBER });
        let lx = x0 + dot * 2.0 + self.s(6.0);
        let lw = g.measure(label, &g.ui_small);
        g.text(label, &g.ui_small, Rect::new(lx, t.rect.y, lw + 2.0, t.rect.h), TEXT);
        let px = lx + lw + self.s(10.0);
        let room = right - px;
        if room < self.s(30.0) {
            return;
        }
        // The text box is as wide as the text, so it starts right after the label.
        let (s, c) = if !v.partial.is_empty() {
            (crate::voice::tail_that_fits(&v.partial, room, |s| g.measure(s, &g.ui_small)), FG)
        } else if live {
            let hint = if v.hold.is_some() { "talk now · let go of Right Alt to type it" } else { "talk now · click the mic or press Esc to stop" };
            (hint.to_string(), TEXT_DIM)
        } else {
            return;
        };
        let w = (g.measure(&s, &g.ui_small) + 2.0).min(room);
        g.text(&s, &g.ui_small, Rect::new(px, t.rect.y, w, t.rect.h), c);
    }

    pub fn paint_toolbars(&self) {
        let g = &self.gfx;
        let (mx, my) = self.ui.mouse;
        let r = self.s(5.0);
        for t in &self.ui.tools {
            let open = self.ui.boxes.iter().any(|b| b.id == t.id);
            // Mic: an icon-only chip. Red while it listens.
            let hot = t.mic.contains(mx, my);
            let (fill, icon) = self.mic_colors(t.id, hot);
            g.round(t.mic, r, fill.unwrap_or(BG), 1.0);
            g.outline(t.mic, r, if fill.is_some() && !hot { fill.unwrap() } else if hot { TEXT_DIM.mix(BORDER, 0.5) } else { BORDER }, 1.0);
            g.mic(t.mic.cx(), t.mic.cy(), t.mic.h * 0.56, icon);
            self.paint_voice_status(t);

            // Rich input: a small text-box icon, the label, and its keys (dimmer).
            let hot = t.rich.contains(mx, my);
            let border = if open { ACCENT.mix(BORDER, 0.45) } else if hot { TEXT_DIM.mix(BORDER, 0.5) } else { BORDER };
            g.round(t.rich, r, if hot { TAB_HOVER } else { BG }, 1.0);
            g.outline(t.rich, r, border, 1.0);
            let fg = if open || hot { TEXT } else { TEXT_DIM };
            let pad = self.s(8.0);
            let iw = self.s(14.0);
            let ih = self.s(10.0);
            let icon = Rect::new((t.rich.x + pad).round(), (t.rich.cy() - ih / 2.0).round(), iw.round(), ih.round());
            g.outline(icon, self.s(2.0), fg, 1.0);
            let th = (1.0 * g.scale).round().max(1.0);
            g.fill(Rect::new(icon.x + self.s(3.0), (icon.cy() - th / 2.0).round(), (iw * 0.45).round(), th), fg);
            let tx = icon.right() + self.s(6.0);
            let lw = g.measure(RICH_LABEL, &g.ui_small);
            g.text(RICH_LABEL, &g.ui_small, Rect::new(tx, t.rich.y, lw + 2.0, t.rich.h), fg);
            let kx = tx + lw + self.s(8.0);
            g.text(RICH_KEYS, &g.ui_small, Rect::new(kx, t.rich.y, t.rich.right() - kx, t.rich.h), TEXT_DIM.mix(BG, 0.25));

            // Folder: a folder icon and the folder's name.
            if t.folder.w > self.s(40.0) {
                let hot = t.folder.contains(mx, my);
                g.round(t.folder, r, if hot { TAB_HOVER } else { BG }, 1.0);
                g.outline(t.folder, r, if hot { TEXT_DIM.mix(BORDER, 0.5) } else { BORDER }, 1.0);
                let fg = if hot { TEXT } else { TEXT_DIM };
                let (iw, ih) = (self.s(14.0).round(), self.s(10.0).round());
                let ix = (t.folder.x + pad).round();
                let iy = (t.folder.cy() - ih / 2.0 + self.s(1.0)).round();
                // The tab on top of the folder, then the body.
                g.fill(Rect::new(ix, iy - self.s(2.0).round(), (iw * 0.45).round(), self.s(2.0).round()), fg);
                g.outline(Rect::new(ix, iy, iw, ih), self.s(1.5), fg, 1.0);
                let tx = ix + iw + self.s(6.0);
                g.text(&self.folder_label(t.id), &g.ui_small, Rect::new(tx, t.folder.y, t.folder.right() - tx - pad / 2.0, t.folder.h), fg);
            }
        }
    }
}
