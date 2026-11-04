//! Small Win32 helpers: clipboard, tray popups, taskbar flash, icons, dialogs.

use windows::core::{w, HSTRING, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::CreateBitmap;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_RESPECT_QUIET_TIME,
    NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, FlashWindowEx, MessageBoxW, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY, HICON, ICONINFO,
    IDYES, MB_ICONWARNING, MB_YESNO, SW_SHOWNORMAL,
};

pub const CF_UNICODETEXT: u32 = 13;
pub const CF_BITMAP: u32 = 2;
pub const CF_DIB: u32 = 8;
pub const CF_DIBV5: u32 = 17;
pub const WM_APP_TRAY: u32 = 0x8000 + 2;

/// UTF-16 with a trailing NUL, for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 without NUL, for DirectWrite.
pub fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

pub fn clipboard_text(hwnd: HWND) -> Option<String> {
    unsafe {
        IsClipboardFormatAvailable(CF_UNICODETEXT).ok()?;
        OpenClipboard(Some(hwnd)).ok()?;
        let out = (|| {
            let h = GetClipboardData(CF_UNICODETEXT).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(g);
            Some(s)
        })();
        let _ = CloseClipboard();
        out
    }
}

pub fn clipboard_has_image() -> bool {
    unsafe {
        IsClipboardFormatAvailable(CF_DIB).is_ok()
            || IsClipboardFormatAvailable(CF_DIBV5).is_ok()
            || IsClipboardFormatAvailable(CF_BITMAP).is_ok()
    }
}

pub fn set_clipboard_text(hwnd: HWND, text: &str) {
    let data = wide(text);
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2) {
            let p = GlobalLock(g) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
                let _ = GlobalUnlock(g);
                let _ = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(g.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

pub fn shell_open(target: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn flash(hwnd: HWND) {
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

/// Windows' "choose a folder" dialog. Returns the folder, or None if cancelled.
pub fn pick_folder(hwnd: HWND, start: Option<&str>) -> Option<String> {
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST).ok()?;
        let _ = dlg.SetTitle(w!("Open Claude in this folder"));
        let _ = dlg.SetOkButtonLabel(w!("Open Claude here"));
        if let Some(s) = start {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(s), None) {
                let _ = dlg.SetFolder(&item);
            }
        }
        // Cancel comes back as an error.
        dlg.Show(Some(hwnd)).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

pub fn confirm(hwnd: HWND, text: &str, title: &str) -> bool {
    unsafe {
        MessageBoxW(Some(hwnd), &HSTRING::from(text), &HSTRING::from(title), MB_YESNO | MB_ICONWARNING) == IDYES
    }
}

fn copy_into(dst: &mut [u16], s: &str) {
    let src: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..src.len()].copy_from_slice(&src);
    dst[src.len()] = 0;
}

pub fn tray_add(hwnd: HWND, icon: HICON) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    copy_into(&mut nid.szTip, "PulseTerm");
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
    }
}

pub fn tray_popup(hwnd: HWND, title: &str, body: &str, warn: bool) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_INFO,
        dwInfoFlags: (if warn { NIIF_WARNING } else { NIIF_INFO }) | NIIF_RESPECT_QUIET_TIME,
        ..Default::default()
    };
    copy_into(&mut nid.szInfoTitle, title);
    copy_into(&mut nid.szInfo, body);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

pub fn tray_remove(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// Dark title bar whose colour matches the tab strip.
pub fn style_title_bar(hwnd: HWND, caption_bgr: u32, text_bgr: u32) {
    unsafe {
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &dark as *const _ as _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption_bgr as *const _ as _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(36), &text_bgr as *const _ as _, 4);
    }
}

/// Make right-click menus dark. Uses uxtheme's unnamed exports 135/136, the same
/// trick Explorer and Notepad use. If they are missing, menus just stay light.
pub fn dark_menus() {
    unsafe {
        let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(lib, PCSTR(135 as *const u8)) {
            let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            set_mode(2);
        }
        if let Some(f) = GetProcAddress(lib, PCSTR(136 as *const u8)) {
            let flush: unsafe extern "system" fn() = std::mem::transmute(f);
            flush();
        }
    }
}

/// The app icon at `size` pixels: the one inside the exe (assets/pulseterm.ico, put there
/// by build.rs), so it is sharp at every size. Falls back to drawing it in code.
pub fn app_icon(size: i32) -> HICON {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{LoadImageW, IMAGE_ICON, LR_DEFAULTCOLOR};
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        // Resource id 1 (see assets/pulseterm.rc). MAKEINTRESOURCE(1) is the number as a pointer.
        if let Ok(h) = LoadImageW(Some(module.into()), PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR) {
            if !h.is_invalid() {
                return HICON(h.0);
            }
        }
    }
    make_icon(size)
}

/// The app icon, drawn in code: an orange rounded square with a white ">_".
/// (Same picture as assets/make_icon.py; used only if the exe has no icon inside.)
pub fn make_icon(size: i32) -> HICON {
    let n = size as usize;
    let s = size as f32;
    let mut px = vec![0u32; n * n];
    let seg = |x: f32, y: f32, ax: f32, ay: f32, bx: f32, by: f32| -> f32 {
        let (dx, dy) = (bx - ax, by - ay);
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((x - ax - t * dx).powi(2) + (y - ay - t * dy).powi(2)).sqrt()
    };
    for yi in 0..n {
        for xi in 0..n {
            let (x, y) = (xi as f32 + 0.5, yi as f32 + 0.5);
            let r = s * 0.22;
            let cx = x.clamp(r, s - r);
            let cy = y.clamp(r, s - r);
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r;
            let a_bg = (0.5 - d).clamp(0.0, 1.0);
            if a_bg <= 0.0 {
                continue;
            }
            let w = s * 0.085;
            let d1 = seg(x, y, s * 0.27, s * 0.30, s * 0.47, s * 0.50).min(seg(x, y, s * 0.47, s * 0.50, s * 0.27, s * 0.70));
            let d2 = seg(x, y, s * 0.53, s * 0.70, s * 0.76, s * 0.70);
            let a_fg = (w - d1.min(d2) + 0.5).clamp(0.0, 1.0);
            let (br, bg, bb) = (0xD9 as f32, 0x77 as f32, 0x57 as f32);
            let r = br + (255.0 - br) * a_fg;
            let g = bg + (255.0 - bg) * a_fg;
            let b = bb + (255.0 - bb) * a_fg;
            let a = a_bg * 255.0;
            // Premultiplied BGRA.
            let pm = |c: f32| (c * a_bg) as u32;
            px[yi * n + xi] = ((a as u32) << 24) | (pm(r) << 16) | (pm(g) << 8) | pm(b);
        }
    }
    unsafe {
        let color = CreateBitmap(size, size, 1, 32, Some(px.as_ptr() as *const _));
        let mask_bits = vec![0u8; n * n / 8 + 4];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const _));
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        CreateIconIndirect(&info).unwrap_or_default()
    }
}

/// Open a file in the editor at a line. `editor` = "code" uses VS Code's `-g`.
pub fn open_in_editor(editor: &str, path: &str, line: Option<u32>, col: Option<u32>) {
    use std::os::windows::process::CommandExt;
    if editor.trim().is_empty() || std::path::Path::new(path).is_dir() {
        shell_open(path);
        return;
    }
    let mut target = path.to_string();
    if let Some(l) = line {
        target.push_str(&format!(":{l}"));
        if let Some(c) = col {
            target.push_str(&format!(":{c}"));
        }
    }
    let r = std::process::Command::new("cmd")
        .args(["/C", editor, "-g", &target])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
    if r.is_err() {
        shell_open(path);
    }
}
