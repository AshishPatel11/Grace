//! Colours for the terminal and the window around it.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb as VRgb};

use crate::gfx::Rgb;

pub const BG: Rgb = Rgb::hex(0x14161B);
pub const FG: Rgb = Rgb::hex(0xD7DAE0);
pub const BAR: Rgb = Rgb::hex(0x0E1014);
pub const TAB_HOVER: Rgb = Rgb::hex(0x1B1E25);
pub const BORDER: Rgb = Rgb::hex(0x262A33);
pub const TEXT: Rgb = Rgb::hex(0xC9CDD4);
pub const TEXT_DIM: Rgb = Rgb::hex(0x7D838E);
pub const ACCENT: Rgb = Rgb::hex(0xD97757);
pub const GREEN: Rgb = Rgb::hex(0x3FB950);
pub const AMBER: Rgb = Rgb::hex(0xD8A03A);
pub const RED: Rgb = Rgb::hex(0xF0605A);
pub const BLUE: Rgb = Rgb::hex(0x5BA4F5);
pub const SELECTION: Rgb = Rgb::hex(0x2F4A73);
pub const CURSOR: Rgb = Rgb::hex(0xE6E8EC);
pub const CARD: Rgb = Rgb::hex(0x1B1E25);

const ANSI: [u32; 16] = [
    0x2A2E36, 0xE06C75, 0x98C379, 0xE5C07B, 0x61AFEF, 0xC678DD, 0x56B6C2, 0xD7DAE0, //
    0x5C6370, 0xF07A84, 0xA9D88A, 0xF0CF8F, 0x7CC0F7, 0xD490E8, 0x6CC9D4, 0xF4F5F7,
];

pub fn indexed(i: u8) -> Rgb {
    let i = i as usize;
    if i < 16 {
        return Rgb::hex(ANSI[i]);
    }
    if i < 232 {
        let n = i - 16;
        let v = |x: usize| if x == 0 { 0 } else { (55 + x * 40) as u8 };
        return Rgb(v(n / 36), v((n / 6) % 6), v(n % 6));
    }
    let g = (8 + (i - 232) * 10) as u8;
    Rgb(g, g, g)
}

/// Colour for one of alacritty's 269 colour slots.
pub fn slot(i: usize) -> Rgb {
    match i {
        0..=255 => indexed(i as u8),
        256 => FG,
        257 => BG,
        258 => CURSOR,
        259..=266 => indexed((i - 259) as u8).mix(BG, 0.34),
        267 => Rgb::hex(0xF4F5F7),
        _ => FG.mix(BG, 0.34),
    }
}

pub fn resolve(c: Color, colors: &Colors) -> Rgb {
    let from = |v: Option<VRgb>| v.map(|r| Rgb(r.r, r.g, r.b));
    match c {
        Color::Spec(r) => Rgb(r.r, r.g, r.b),
        Color::Named(n) => from(colors[n as usize]).unwrap_or_else(|| slot(n as usize)),
        Color::Indexed(i) => from(colors[i as usize]).unwrap_or_else(|| indexed(i)),
    }
}

pub fn is_default_bg(c: Color) -> bool {
    matches!(c, Color::Named(NamedColor::Background))
}

pub fn to_vrgb(c: Rgb) -> VRgb {
    VRgb { r: c.0, g: c.1, b: c.2 }
}
