//! Turn special keys (arrows, F-keys, …) into the byte codes terminal programs expect.

pub const VK_BACK: u16 = 0x08;
pub const VK_TAB: u16 = 0x09;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_SPACE: u16 = 0x20;
pub const VK_PRIOR: u16 = 0x21;
pub const VK_NEXT: u16 = 0x22;
pub const VK_END: u16 = 0x23;
pub const VK_HOME: u16 = 0x24;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DOWN: u16 = 0x28;
pub const VK_INSERT: u16 = 0x2D;
pub const VK_DELETE: u16 = 0x2E;
pub const VK_F1: u16 = 0x70;
pub const VK_F10: u16 = 0x79;
pub const VK_F12: u16 = 0x7B;
pub const VK_OEM_PLUS: u16 = 0xBB;
pub const VK_OEM_COMMA: u16 = 0xBC;
pub const VK_OEM_MINUS: u16 = 0xBD;
pub const VK_OEM_2: u16 = 0xBF;
pub const VK_ADD: u16 = 0x6B;
pub const VK_SUBTRACT: u16 = 0x6D;

/// xterm modifier number: 1 + shift + 2*alt + 4*ctrl.
fn modnum(shift: bool, alt: bool, ctrl: bool) -> u8 {
    1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8
}

/// Bytes for a non-character key, or None if the key types a character
/// (Windows then sends WM_CHAR with the right letter for the keyboard layout).
pub fn special(vk: u16, shift: bool, ctrl: bool, alt: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let m = modnum(shift, alt, ctrl);
    let csi = |s: &str| format!("\x1b[{s}").into_bytes();
    let cursor = |c: char| {
        if m > 1 {
            csi(&format!("1;{m}{c}"))
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            csi(&c.to_string())
        }
    };
    let tilde = |n: u8| if m > 1 { csi(&format!("{n};{m}~")) } else { csi(&format!("{n}~")) };
    Some(match vk {
        VK_UP => cursor('A'),
        VK_DOWN => cursor('B'),
        VK_RIGHT => cursor('C'),
        VK_LEFT => cursor('D'),
        VK_HOME => cursor('H'),
        VK_END => cursor('F'),
        VK_INSERT => tilde(2),
        VK_DELETE => tilde(3),
        VK_PRIOR => tilde(5),
        VK_NEXT => tilde(6),
        0x70..=0x73 => {
            let c = (b'P' + (vk - 0x70) as u8) as char;
            if m > 1 {
                csi(&format!("1;{m}{c}"))
            } else {
                format!("\x1bO{c}").into_bytes()
            }
        }
        0x74..=0x7B => {
            let n = [15u8, 17, 18, 19, 20, 21, 23, 24][(vk - 0x74) as usize];
            tilde(n)
        }
        VK_BACK => {
            if ctrl {
                vec![0x08]
            } else if alt {
                vec![0x1b, 0x7f]
            } else {
                vec![0x7f]
            }
        }
        VK_TAB if shift && !ctrl => csi("Z"),
        // Shift+Enter / Alt+Enter = a new line inside Claude's prompt (sends Esc + Enter).
        VK_RETURN if shift || alt => vec![0x1b, b'\r'],
        VK_SPACE if ctrl => vec![0],
        VK_OEM_2 if ctrl && !shift => vec![0x1f],
        _ => return None,
    })
}
