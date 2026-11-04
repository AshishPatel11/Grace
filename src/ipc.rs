//! A local pipe other apps (claude-snap, tests) use to talk to Grace.
//!
//! Protocol: the client writes one JSON line, Grace answers one JSON line.
//!   {"cmd":"list"}                                  -> tabs with title, status, RAM
//!   {"cmd":"paste","tab":2,"text":"why?","image":true,"submit":false}
//!   {"cmd":"send","tab":2,"text":"ls\r"}            -> raw keys into the tab
//!   {"cmd":"dump","tab":2}                          -> the visible screen as text
//!   {"cmd":"focus","tab":2}   {"cmd":"new_tab","profile":"PowerShell","cwd":"C:\\"}
//! `tab` is 1-based, like the Ctrl+1..9 shortcuts. Leave it out for the active tab.

use std::io::{Read, Write};
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{FlushFileBuffers, ReadFile, WriteFile, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};

use crate::pane::{wake, AppEvent};

pub struct Request {
    pub json: serde_json::Value,
    pub reply: Sender<String>,
}

fn pipe_for(app: &str) -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let dev = if crate::dev() { "dev-" } else { "" };
    format!(r"\\.\pipe\{app}-{dev}{}", user.to_ascii_lowercase())
}

/// `\\.\pipe\grace-<user>` (`grace-dev-<user>` for the test copy).
pub fn pipe_name() -> String {
    pipe_for("grace")
}

/// The name from before the app was renamed (it was PulseTerm). Still answered, so
/// tools made for the old name, like ClaudeSnap, keep working without a change.
fn old_pipe_name() -> String {
    pipe_for("pulseterm")
}

pub fn start_server(tx: Sender<AppEvent>) {
    for name in [pipe_name(), old_pipe_name()] {
        let tx = tx.clone();
        std::thread::Builder::new().name("ipc".into()).spawn(move || serve(&name, tx)).ok();
    }
}

/// Answer requests on one pipe name, one client at a time, forever.
fn serve(name: &str, tx: Sender<AppEvent>) {
    let name = HSTRING::from(name);
    loop {
        let h = unsafe {
            CreateNamedPipeW(
                &name,
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                255,
                65536,
                65536,
                0,
                None,
            )
        };
        if h == INVALID_HANDLE_VALUE || h.is_invalid() {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        unsafe {
            let _ = ConnectNamedPipe(h, None);
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let mut n = 0u32;
                if ReadFile(h, Some(&mut buf), Some(&mut n), None).is_err() || n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n as usize]);
                if data.contains(&b'\n') || data.len() > 8 << 20 {
                    break;
                }
            }
            let line = String::from_utf8_lossy(&data).trim().to_string();
            let answer = match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(json) => {
                    let (rtx, rrx) = channel();
                    if tx.send(AppEvent::Ipc(Request { json, reply: rtx })).is_err() {
                        return;
                    }
                    wake();
                    rrx.recv_timeout(Duration::from_secs(5))
                        .unwrap_or_else(|_| r#"{"ok":false,"error":"timeout"}"#.into())
                }
                Err(e) => format!(r#"{{"ok":false,"error":"bad json: {}"}}"#, e.to_string().replace('"', "'")),
            };
            let out = format!("{answer}\n");
            let mut written = 0u32;
            let _ = WriteFile(h, Some(out.as_bytes()), Some(&mut written), None);
            let _ = FlushFileBuffers(h);
            let _ = DisconnectNamedPipe(h);
            let _ = CloseHandle(h);
        }
    }
}

/// Send one request to a running Grace. None if it is not running.
pub fn send(json: &str) -> Option<String> {
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(pipe_name()).ok()?;
    f.write_all(json.trim().as_bytes()).ok()?;
    f.write_all(b"\n").ok()?;
    f.flush().ok()?;
    let mut out = String::new();
    let mut buf = [0u8; 65536];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.ends_with('\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    Some(out.trim().to_string())
}
