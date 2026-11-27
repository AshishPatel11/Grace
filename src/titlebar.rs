//! Tabs in the title bar: Grace draws the whole top of the window itself.
//!
//! Windows' own title bar is removed (WM_NCCALCSIZE in main.rs keeps only the side and
//! bottom resize borders), so the tab strip sits at the very top, like Chrome and Windows
//! Terminal. Grace draws minimize / maximize / close and tells Windows what each part of
//! the strip is (WM_NCHITTEST): the free space drags the window (double-click maximizes,
//! right-click opens the window menu), the top edge resizes, and the maximize button is
//! reported as one so Windows 11 shows its snap layouts when you hover it.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    IsZoomed, PostMessageW, ShowWindow, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE,
    WM_CLOSE,
};

use crate::app::App;
use crate::gfx::Rgb;
use crate::layout::Rect;
use crate::theme::*;

/// Width of one window button (Windows 11 uses 46 px at 100%).
pub const CAP_W: f32 = 46.0;
/// Hit-test codes of the three buttons, in drawing order.
pub const CAP_HITS: [u32; 3] = [HTMINBUTTON, HTMAXBUTTON, HTCLOSE];
/// The close button's hover colour (Windows' own red).
const CLOSE_RED: Rgb = Rgb::hex(0xC42B1C);

pub fn maximized(hwnd: HWND) -> bool {
    unsafe { IsZoomed(hwnd).as_bool() }
}

impl App {
    /// What the point (window pixels) is, for WM_NCHITTEST. Only the strip at the top
    /// is not plain window content.
    pub fn caption_hit(&self, x: f32, y: f32) -> u32 {
        if y >= self.bar_h() {
            return HTCLIENT;
        }
        if let Some(i) = self.ui.caption.iter().position(|r| r.contains(x, y)) {
            return CAP_HITS[i];
        }
        let on_control = self.ui.tabs.iter().any(|t| t.rect.contains(x, y))
            || self.ui.plus.contains(x, y)
            || self.ui.menu.contains(x, y)
            || self.ui.sys.contains(x, y);
        if on_control || self.ui.rename.is_some() {
            HTCLIENT
        } else {
            HTCAPTION
        }
    }

    /// The mouse is over a window button (or left them: 0). Returns true when that changed.
    pub fn caption_hover(&mut self, hit: u32) -> bool {
        let hit = if CAP_HITS.contains(&hit) { hit } else { 0 };
        if self.ui.cap_hover == hit {
            return false;
        }
        self.ui.cap_hover = hit;
        self.hover_fade.set(if hit == 0 { 0 } else { hit as u64 | 1 << 40 });
        self.invalidate();
        true
    }

    pub fn caption_press(&mut self, hit: u32) -> bool {
        if !CAP_HITS.contains(&hit) {
            return false;
        }
        self.ui.cap_press = hit;
        self.invalidate();
        true
    }

    /// The button came up: act if it is still on the button it went down on.
    pub fn caption_release(&mut self, hit: u32) -> bool {
        let pressed = std::mem::take(&mut self.ui.cap_press);
        self.invalidate();
        if pressed == 0 || pressed != hit {
            return pressed != 0;
        }
        unsafe {
            match hit {
                HTMINBUTTON => {
                    let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
                }
                HTMAXBUTTON => {
                    let _ = ShowWindow(self.hwnd, if maximized(self.hwnd) { SW_RESTORE } else { SW_MAXIMIZE });
                }
                HTCLOSE => {
                    let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, Default::default(), Default::default());
                }
                _ => {}
            }
        }
        self.ui.cap_hover = 0;
        true
    }

    /// Where the three buttons go: the top-right corner, the strip's full height.
    pub fn layout_caption(&mut self) {
        let (w, _) = self.client();
        let s = self.gfx.scale;
        let bw = (CAP_W * s).round();
        let h = self.bar_h() - 1.0;
        for i in 0..3 {
            self.ui.caption[i] = Rect::new(w - bw * (3 - i) as f32, 0.0, bw, h);
        }
    }

    pub fn paint_caption(&self) {
        let g = &self.gfx;
        let s = g.scale;
        let max = maximized(self.hwnd);
        let dim = !self.focused;
        for (i, r) in self.ui.caption.iter().enumerate() {
            let hit = CAP_HITS[i];
            let close = hit == HTCLOSE;
            let hot = self.ui.cap_hover == hit;
            let down = self.ui.cap_press == hit && hot;
            let t = self.hover_fade.amount(hit as u64 | 1 << 40);
            if t > 0.0 || down {
                let (c, a) = if close { (CLOSE_RED, if down { 0.8 } else { t }) } else { (TEXT, if down { 0.09 } else { 0.06 * t }) };
                g.fill_a(*r, c, a);
            }
            let ink = if close && hot { Rgb::hex(0xFFFFFF).mix(TEXT, 1.0 - t) } else if dim { TEXT_DIM.mix(BAR, 0.3) } else { TEXT };
            let (cx, cy) = (r.cx().round() + 0.5, r.cy().round() + 0.5);
            let k = (5.0 * s).round();
            let lw = 1.0 * s;
            match hit {
                HTMINBUTTON => g.line(cx - k, cy, cx + k, cy, lw, ink),
                HTMAXBUTTON if max => {
                    // Restore: two overlapping squares.
                    let o = (2.0 * s).round();
                    g.outline(Rect::new(cx - k, cy - k + o, 2.0 * k - o, 2.0 * k - o), 1.5 * s, ink, lw);
                    g.line(cx - k + o, cy - k, cx + k, cy - k, lw, ink);
                    g.line(cx + k, cy - k, cx + k, cy + k - o, lw, ink);
                }
                HTMAXBUTTON => g.outline(Rect::new(cx - k, cy - k, 2.0 * k, 2.0 * k), 1.5 * s, ink, lw),
                _ => {
                    g.line(cx - k, cy - k, cx + k, cy + k, lw, ink);
                    g.line(cx + k, cy - k, cx - k, cy + k, lw, ink);
                }
            }
        }
    }
}
