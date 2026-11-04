//! PulseTerm: a light terminal for Claude Code on Windows.
#![windows_subsystem = "windows"]

mod app;
mod compose;
mod composebox;
mod config;
mod folders;
mod gfx;
mod images;
mod input;
mod ipc;
mod layout;
mod links;
mod pane;
mod predict;
mod preview;
mod procmon;
mod render;
mod theme;
mod thumbs;
mod blink;
mod toolbar;
mod voice;
mod win;

use std::cell::RefCell;
use std::io::Write;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, ScreenToClient, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::Ime::{ImmGetContext, ImmReleaseContext, ImmSetCompositionWindow, CFS_POINT, COMPOSITIONFORM};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

use app::{App, Deferred};

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

const TIMER_BLINK: usize = 1;

/// `--dev`: a separate test copy with its own window, pipe and settings folder,
/// so testing a new build never touches the PulseTerm you are working in.
static DEV: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn dev() -> bool {
    DEV.load(std::sync::atomic::Ordering::Relaxed)
}

fn class() -> PCWSTR {
    if dev() { w!("PulseTermDevWindow") } else { w!("PulseTermWindow") }
}

pub fn log(msg: &str) {
    let path = config::data_dir().join("pulseterm.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{:?} {msg}", std::time::SystemTime::now());
    }
}

/// Run `f` on the app if it is not already in use. Win32 sometimes calls the
/// window procedure again from inside one of our own calls; then we just let
/// Windows do its default thing instead of touching the app twice.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| match a.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => None,
    })
}

/// Menus and yes/no boxes run their own message loop, so they must run
/// while nobody holds the app.
fn run_deferred(hwnd: HWND) {
    loop {
        let Some(Some(d)) = with_app(|a| a.pending.pop()) else { return };
        match d {
            Deferred::Menu { items, x, y, ctx } => unsafe {
                let Some(menu) = build_menu(&items) else { continue };
                let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, None, hwnd, None);
                let _ = DestroyMenu(menu);
                with_app(|a| a.menu_command(ctx, cmd.0 as u32));
            },
            Deferred::Confirm { text, then } => {
                let yes = win::confirm(hwnd, &text, "PulseTerm");
                with_app(|a| a.after_confirm(then, yes));
            }
            Deferred::PickFolder { start } => {
                if let Some(dir) = win::pick_folder(hwnd, start.as_deref()) {
                    with_app(|a| a.open_claude_in(&dir));
                }
            }
        }
    }
}

fn lparam_xy(lp: LPARAM) -> (f32, f32) {
    let x = (lp.0 & 0xFFFF) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let r = handle(hwnd, msg, wp, lp);
    run_deferred(hwnd);
    match r {
        Some(v) => LRESULT(v),
        None => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<isize> {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            with_app(|a| a.paint());
            let _ = EndPaint(hwnd, &ps);
            Some(0)
        }
        WM_ERASEBKGND => Some(1),
        WM_SIZE if wp.0 == 1 => Some(0), // minimized: keep the terminals at their size
        WM_SIZE => {
            let w = (lp.0 & 0xFFFF) as u32;
            let h = ((lp.0 >> 16) & 0xFFFF) as u32;
            with_app(|a| {
                a.gfx.resize(w, h);
                a.relayout();
                a.invalidate();
            })?;
            Some(0)
        }
        pane::WM_APP_WAKE => {
            with_app(|a| a.drain());
            Some(0)
        }
        win::WM_APP_TRAY => {
            let ev = (lp.0 & 0xFFFF) as u32;
            // NIN_BALLOONUSERCLICK = 0x405, NIN_SELECT = 0x400, WM_LBUTTONUP
            if ev == 0x405 || ev == 0x400 || ev == WM_LBUTTONUP {
                with_app(|a| a.on_tray());
            }
            Some(0)
        }
        WM_TIMER if wp.0 == TIMER_BLINK => {
            // The blink's next step is due: draw again. Painting works out how the cursor
            // looks from the clock (blink.rs) and sets the timer for the step after.
            let _ = KillTimer(Some(hwnd), TIMER_BLINK);
            with_app(|a| a.invalidate());
            Some(0)
        }
        WM_TIMER if wp.0 == 2 => {
            let _ = KillTimer(Some(hwnd), 2);
            with_app(|a| {
                a.toast = None;
                a.invalidate();
            });
            Some(0)
        }
        WM_CHAR => {
            with_app(|a| a.on_char(wp.0 as u16, false));
            Some(0)
        }
        WM_SYSCHAR => {
            if wp.0 as u16 == b' ' as u16 {
                return None; // Alt+Space opens the window menu
            }
            with_app(|a| a.on_char(wp.0 as u16, true));
            Some(0)
        }
        WM_SYSKEYUP if wp.0 as u16 == input::VK_MENU || wp.0 as u16 == input::VK_F10 => Some(0),
        WM_KEYUP if wp.0 as u16 == input::VK_CONTROL => {
            with_app(|a| {
                a.ui.link = None;
                a.invalidate()
            });
            None
        }
        WM_SETFOCUS => {
            with_app(|a| {
                a.on_focus(true);
                a.invalidate();
            });
            Some(0)
        }
        WM_KILLFOCUS => {
            with_app(|a| a.on_focus(false));
            let _ = KillTimer(Some(hwnd), TIMER_BLINK);
            Some(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let (x, y) = lparam_xy(lp);
            let b = match msg {
                WM_LBUTTONDOWN => 0,
                WM_MBUTTONDOWN => 1,
                _ => 2,
            };
            with_app(|a| {
                a.blink_reset();
                a.on_mouse_down(x, y, b)
            });
            Some(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = lparam_xy(lp);
            let b = match msg {
                WM_LBUTTONUP => 0,
                WM_MBUTTONUP => 1,
                _ => 2,
            };
            with_app(|a| a.on_mouse_up(x, y, b));
            Some(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = lparam_xy(lp);
            with_app(|a| {
                if !a.ui.tracking {
                    let mut t = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut t);
                    a.ui.tracking = true;
                }
                a.on_mouse_move(x, y)
            });
            Some(0)
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            with_app(|a| {
                a.ui.tracking = false;
                a.ui.mouse = (-1.0, -1.0);
                a.ui.preview = None;
                a.invalidate();
            });
            Some(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
            let (sx, sy) = lparam_xy(lp);
            let mut p = windows::Win32::Foundation::POINT { x: sx as i32, y: sy as i32 };
            let _ = ScreenToClient(hwnd, &mut p);
            with_app(|a| a.on_wheel(p.x as f32, p.y as f32, delta));
            Some(0)
        }
        WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
            let c = with_app(|a| a.cursor_for(a.ui.mouse.0, a.ui.mouse.1)).unwrap_or(0);
            let id = match c {
                1 => IDC_IBEAM,
                2 => IDC_HAND,
                3 => IDC_SIZEWE,
                4 => IDC_SIZENS,
                _ => IDC_ARROW,
            };
            if let Ok(cur) = LoadCursorW(None, id) {
                SetCursor(Some(cur));
            }
            Some(1)
        }
        WM_IME_STARTCOMPOSITION => {
            if let Some(Some((x, y))) = with_app(|a| a.ime_point()) {
                let himc = ImmGetContext(hwnd);
                let cf = COMPOSITIONFORM {
                    dwStyle: CFS_POINT,
                    ptCurrentPos: windows::Win32::Foundation::POINT { x: x as i32, y: y as i32 },
                    rcArea: RECT::default(),
                };
                let _ = ImmSetCompositionWindow(himc, &cf);
                let _ = ImmReleaseContext(hwnd, himc);
            }
            None
        }
        WM_DROPFILES => {
            let hdrop = HDROP(wp.0 as *mut _);
            let n = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
            let mut files = Vec::new();
            for i in 0..n {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, i, Some(&mut buf));
                files.push(String::from_utf16_lossy(&buf[..len]));
            }
            DragFinish(hdrop);
            with_app(|a| a.on_drop(files));
            Some(0)
        }
        WM_DPICHANGED => {
            let dpi = (wp.0 & 0xFFFF) as f32;
            let r = &*(lp.0 as *const RECT);
            with_app(|a| a.set_scale(dpi / 96.0));
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            with_app(|a| {
                let (w, h) = a.client();
                a.gfx.resize(w as u32, h as u32);
                a.relayout();
                a.invalidate();
            });
            Some(0)
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lp.0 as *mut MINMAXINFO);
            mmi.ptMinTrackSize.x = 420;
            mmi.ptMinTrackSize.y = 260;
            Some(0)
        }
        WM_CLOSE => {
            let ok = with_app(|a| a.on_close()).unwrap_or(true);
            if ok {
                let _ = DestroyWindow(hwnd);
            }
            Some(0)
        }
        WM_DESTROY => {
            with_app(|a| a.shutdown());
            PostQuitMessage(0);
            Some(0)
        }
        _ => None,
    }
}

/// If PulseTerm was started from inside a Claude Code session, these markers
/// would make every Claude in our tabs think it is a sub-agent (no saved chat,
/// no status file). Only session markers are removed, never user settings.
/// The environment a program gets when started from the Start menu: only the user's and the
/// machine's saved variables, nothing a parent program added.
fn fresh_user_env() -> Option<Vec<(String, String)>> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut block: *mut core::ffi::c_void = std::ptr::null_mut();
        let made = CreateEnvironmentBlock(&mut block, Some(token), false);
        let _ = CloseHandle(token);
        made.ok()?;
        // The block is "NAME=value\0NAME=value\0\0" in UTF-16.
        let mut out = Vec::new();
        let mut p = block as *const u16;
        loop {
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            if len == 0 {
                break;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            p = p.add(len + 1);
            // Skip the hidden "=C:=C:\..." entries; they start with '='.
            if let Some(i) = s.get(1..).and_then(|rest| rest.find('=')) {
                out.push((s[..i + 1].to_string(), s[i + 2..].to_string()));
            }
        }
        let _ = DestroyEnvironmentBlock(block);
        if out.is_empty() { None } else { Some(out) }
    }
}

fn strip_parent_session_env() {
    // Started from inside a Claude Code command (by Claude itself, or a script it runs)?
    // Those commands carry extra settings: NO_COLOR=1 (every Claude in every tab would draw
    // without colour), GIT_EDITOR=true (git commit would skip the editor), and more.
    // So start from the clean Start-menu environment instead. A NO_COLOR the user saved in
    // Windows on purpose is part of that clean set, so it still works.
    if std::env::var_os("CLAUDECODE").is_some() {
        match fresh_user_env() {
            Some(fresh) => {
                let mut keep: std::collections::HashSet<String> = fresh.iter().map(|(k, _)| k.to_ascii_uppercase()).collect();
                // Windows sets these at sign-in, not in the saved set, so the clean set lacks
                // them. Keep them (the pipe name uses USERNAME).
                for k in ["USERNAME", "USERDOMAIN", "USERDOMAIN_ROAMINGPROFILE", "LOGONSERVER", "SESSIONNAME", "CLIENTNAME", "COMPUTERNAME"] {
                    keep.insert(k.into());
                }
                for (k, _) in std::env::vars_os() {
                    let name = k.to_string_lossy().to_ascii_uppercase();
                    if !keep.contains(&name) {
                        // SAFETY: called first thing in main, before any other thread starts.
                        unsafe { std::env::remove_var(&k) };
                    }
                }
                for (k, v) in fresh {
                    unsafe { std::env::set_var(k, v) };
                }
            }
            None => {
                for k in ["NO_COLOR", "FORCE_COLOR", "GIT_EDITOR", "GIT_TERMINAL_PROMPT", "GIT_ASKPASS"] {
                    unsafe { std::env::remove_var(k) };
                }
            }
        }
    }
    for k in [
        "CLAUDECODE",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_SESSION_ATTENDED",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_EXECPATH",
        "CLAUDE_CODE_VERSION",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_PID",
        "CLAUDE_EFFORT",
    ] {
        // SAFETY: called first thing in main, before any other thread starts.
        unsafe { std::env::remove_var(k) };
    }
}

fn main() {
    // A crash in a release build closes the window silently. Write the reason to the log first.
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        log(&format!("CRASH: {info}
{bt}"));
    }));
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--dev") || std::env::var_os("PULSETERM_DEV").is_some() {
        DEV.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    strip_parent_session_env();

    // `pulseterm --send '{"cmd":"list"}'` talks to the running window (for tests and scripts).
    if args.len() >= 3 && args[1] == "--send" {
        let out = ipc::send(&args[2]).unwrap_or_else(|| r#"{"ok":false,"error":"PulseTerm is not running"}"#.into());
        attach_console();
        println!("{out}");
        return;
    }

    // `pulseterm --resume <chat id>` opens a tab that continues that Claude chat.
    let resume = args.iter().position(|a| a == "--resume").and_then(|i| args.get(i + 1)).cloned();

    // Only one window: a second launch opens a new tab in the first one.
    let here = std::env::current_dir().ok().map(|d| d.to_string_lossy().to_string());
    if let Some(h) = unsafe { FindWindowW(class(), PCWSTR::null()) }.ok() {
        let req = match &resume {
            Some(id) => serde_json::json!({"cmd": "new_tab", "resume": id, "cwd": here}).to_string(),
            None => serde_json::json!({"cmd": "new_tab", "cwd": here}).to_string(),
        };
        if ipc::send(&req).is_some() {
            unsafe {
                if IsIconic(h).as_bool() {
                    let _ = ShowWindow(h, SW_RESTORE);
                }
                let _ = SetForegroundWindow(h);
            }
            return;
        }
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        win::dark_menus();
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let icon = win::app_icon(GetSystemMetrics(SM_CXICON));
        let small = win::app_icon(GetSystemMetrics(SM_CXSMICON));
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: icon,
            hIconSm: small,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class(),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let saved = App::saved_window();
        let (x, y, w, h) = match saved {
            Some(s) if s.w > 200 && s.h > 150 => (s.x, s.y, s.w, s.h),
            _ => (CW_USEDEFAULT, CW_USEDEFAULT, 1200, 780),
        };
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class(),
            w!("PulseTerm"),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            log("CreateWindowExW failed");
            return;
        };
        pane::set_main_hwnd(hwnd);
        win::style_title_bar(hwnd, theme::BAR.bgr(), theme::TEXT.bgr());
        DragAcceptFiles(hwnd, true);

        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let cfg = config::load_config();
        let app = match App::new(hwnd, cfg, scale.max(1.0), icon) {
            Ok(a) => a,
            Err(e) => {
                log(&format!("start failed: {e}"));
                return;
            }
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
        win::tray_add(hwnd, small);

        let show = if saved.is_some_and(|s| s.maximized) { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL };
        if dev() {
            // The test copy opens behind other windows and never takes the keyboard,
            // so it cannot catch keys meant for the real PulseTerm.
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        } else {
            let _ = ShowWindow(hwnd, show);
        }
        with_app(|a| {
            let (w, h) = a.client();
            a.gfx.resize(w as u32, h as u32);
            a.relayout();
            match &resume {
                Some(id) => {
                    a.restore_or_new(false);
                    a.resume_chat(id, here.clone());
                }
                None => a.restore_or_new(true),
            }
        });
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // Push to talk: Right Alt down starts the mic, up stops it and types the words.
            // The key then goes on as usual (Alt alone does nothing else here).
            if let Some(down) = voice::right_alt(msg.message, msg.wParam.0, msg.lParam.0) {
                with_app(|a| if down { a.voice_hold_down() } else { a.voice_hold_up() });
            }
            if msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN {
                let vk = msg.wParam.0 as u16;
                let is_alt_f4 = msg.message == WM_SYSKEYDOWN && vk == 0x73;
                if !is_alt_f4 && with_app(|a| a.on_keydown(vk)) == Some(true) {
                    run_deferred(hwnd);
                    continue;
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
    }
}

fn attach_console() {
    // A GUI program has no console; borrow the parent's so `--send` can print.
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    unsafe {
        AttachConsole(0xFFFF_FFFF);
    }
}

/// Makes a Windows popup menu from (id, label) items. Id 0 is a separator line;
/// `app::SUB_START` / `app::SUB_END` open and close a submenu named by SUB_START's label.
fn build_menu(items: &[(u32, String)]) -> Option<HMENU> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        // `stack`: the menus being filled; the last one gets the next items.
        // A submenu is added to its parent when it closes.
        let mut stack: Vec<(HMENU, String)> = vec![(menu, String::new())];
        for (id, label) in items {
            let cur = stack.last().unwrap().0;
            match *id {
                0 => {
                    let _ = AppendMenuW(cur, MF_SEPARATOR, 0, PCWSTR::null());
                }
                app::SUB_START => {
                    if let Ok(sub) = CreatePopupMenu() {
                        stack.push((sub, label.clone()));
                    }
                }
                app::SUB_END if stack.len() > 1 => {
                    let (sub, name) = stack.pop().unwrap();
                    let parent = stack.last().unwrap().0;
                    let _ = AppendMenuW(parent, MF_POPUP, sub.0 as usize, &HSTRING::from(name.as_str()));
                }
                app::SUB_END => {}
                _ => {
                    let _ = AppendMenuW(cur, MF_STRING, *id as usize, &HSTRING::from(label.as_str()));
                }
            }
        }
        // A submenu left open by mistake is still shown.
        while stack.len() > 1 {
            let (sub, name) = stack.pop().unwrap();
            let _ = AppendMenuW(stack.last().unwrap().0, MF_POPUP, sub.0 as usize, &HSTRING::from(name.as_str()));
        }
        Some(menu)
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    #[test]
    fn submenus_are_nested() {
        let items = vec![
            (1, "Rename".to_string()),
            (app::SUB_START, "Split right with tab".to_string()),
            (1001, "2  Claude".to_string()),
            (1002, "3  PowerShell".to_string()),
            (app::SUB_END, String::new()),
            (0, String::new()),
            (6, "Close".to_string()),
        ];
        unsafe {
            let m = build_menu(&items).unwrap();
            // Rename, the submenu, a separator, Close.
            assert_eq!(GetMenuItemCount(Some(m)), 4);
            let sub = GetSubMenu(m, 1);
            assert!(!sub.is_invalid());
            assert_eq!(GetMenuItemCount(Some(sub)), 2);
            assert_eq!(GetMenuItemID(sub, 0), 1001);
            assert_eq!(GetMenuItemID(sub, 1), 1002);
            assert_eq!(GetMenuItemID(m, 3), 6);
            let mut buf = [0u16; 64];
            let n = GetMenuStringW(m, 1, Some(&mut buf), MF_BYPOSITION);
            assert_eq!(String::from_utf16_lossy(&buf[..n as usize]), "Split right with tab");
            let _ = DestroyMenu(m);
        }
    }
}
