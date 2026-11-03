//! Background thread: once a second, read RAM per tab and Claude's status.
//!
//! Cost: one Windows call that lists all processes (~1-2 ms), plus reading a
//! few tiny JSON files Claude Code already writes. No processes are started.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

use crate::pane::{wake, AppEvent};

#[derive(Clone, Debug, Default)]
pub struct ClaudeInfo {
    pub pid: u32,
    pub session_id: String,
    pub name: String,
    /// "busy", "idle", "asking" or "shell".
    pub status: String,
    pub waiting_for: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Default)]
pub struct PaneStats {
    /// Private memory of the whole process tree, in bytes.
    pub mem: u64,
    /// Biggest processes in the tree: (name, bytes).
    pub top: Vec<(String, u64)>,
    pub claude: Option<ClaudeInfo>,
}

#[derive(Clone, Debug, Default)]
pub struct SysMem {
    pub total: u64,
    pub avail: u64,
    pub commit_limit: u64,
    pub commit_used: u64,
    pub self_mem: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub panes: Vec<(u64, PaneStats)>,
    pub sys: SysMem,
}

struct Proc {
    ppid: u32,
    name: String,
    private: u64,
    created: i64,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(class: u32, buf: *mut core::ffi::c_void, len: u32, ret: *mut u32) -> i32;
}

fn processes(buf: &mut Vec<u8>) -> HashMap<u32, Proc> {
    let mut out = HashMap::new();
    unsafe {
        loop {
            let mut ret = 0u32;
            let st = NtQuerySystemInformation(5, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut ret);
            if st == 0xC0000004u32 as i32 {
                let n = (ret as usize + 65536).max(buf.len() * 2);
                buf.resize(n, 0);
                continue;
            }
            if st != 0 {
                return out;
            }
            break;
        }
        let mut off = 0usize;
        loop {
            let p = buf.as_ptr().add(off);
            let next = *(p as *const u32) as usize;
            let created = *(p.add(0x20) as *const i64);
            let name_len = *(p.add(0x38) as *const u16) as usize / 2;
            let name_ptr = *(p.add(0x40) as *const *const u16);
            let name = if name_ptr.is_null() {
                String::new()
            } else {
                String::from_utf16_lossy(std::slice::from_raw_parts(name_ptr, name_len))
            };
            let pid = *(p.add(0x50) as *const usize) as u32;
            let ppid = *(p.add(0x58) as *const usize) as u32;
            // PagefileUsage = private bytes, the "Commit size" column in Task Manager.
            let private = *(p.add(0xB8) as *const usize) as u64;
            out.insert(pid, Proc { ppid, name, private, created });
            if next == 0 {
                break;
            }
            off += next;
        }
    }
    out
}

fn sys_mem(self_mem: u64) -> SysMem {
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe {
        let _ = GlobalMemoryStatusEx(&mut m);
    }
    SysMem {
        total: m.ullTotalPhys,
        avail: m.ullAvailPhys,
        commit_limit: m.ullTotalPageFile,
        commit_used: m.ullTotalPageFile.saturating_sub(m.ullAvailPageFile),
        self_mem,
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct SessionFile {
    session_id: String,
    name: Option<String>,
    status: Option<String>,
    waiting_for: Option<String>,
    cwd: Option<String>,
}

pub fn claude_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return d.into();
    }
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude")
}

fn read_session(pid: u32) -> Option<ClaudeInfo> {
    let path = claude_dir().join("sessions").join(format!("{pid}.json"));
    let text = std::fs::read_to_string(path).ok()?;
    let f: SessionFile = serde_json::from_str(&text).ok()?;
    let waiting = f.waiting_for.unwrap_or_default();
    let status = if !waiting.is_empty() { "asking".to_string() } else { f.status.unwrap_or_else(|| "idle".into()) };
    Some(ClaudeInfo {
        pid,
        session_id: f.session_id,
        name: f.name.unwrap_or_default(),
        status,
        waiting_for: waiting,
        cwd: f.cwd.unwrap_or_default(),
    })
}

/// `targets` = (pane id, process id of the program the pane started).
pub fn start(targets: Arc<Mutex<Vec<(u64, u32)>>>, tx: Sender<AppEvent>) {
    std::thread::Builder::new()
        .name("procmon".into())
        .spawn(move || {
            let mut buf = vec![0u8; 1 << 20];
            let me = std::process::id();
            // The full process list is the costly part, so it runs every 4th second.
            // Claude's status files are tiny, so they are read every second (fast alerts).
            let mut last: Snapshot = Snapshot::default();
            let mut last_list: Vec<(u64, u32)> = Vec::new();
            let mut tick: u64 = 0;
            loop {
                let list = targets.lock().unwrap().clone();
                let full = tick % 4 == 0 || list != last_list;
                tick += 1;
                if !full {
                    for (_, st) in last.panes.iter_mut() {
                        if let Some(c) = &st.claude {
                            st.claude = read_session(c.pid);
                        }
                    }
                    if tx.send(AppEvent::Stats(last.clone())).is_err() {
                        return;
                    }
                    wake();
                    std::thread::sleep(Duration::from_millis(1000));
                    continue;
                }
                last_list = list.clone();
                let procs = processes(&mut buf);
                let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
                for (pid, p) in &procs {
                    kids.entry(p.ppid).or_default().push(*pid);
                }
                let mut snap = Snapshot::default();
                for (pane, root) in list {
                    let mut st = PaneStats::default();
                    let Some(rp) = procs.get(&root) else {
                        snap.panes.push((pane, st));
                        continue;
                    };
                    let mut stack = vec![(root, rp.created)];
                    let mut claude: Option<(u32, i64)> = None;
                    let mut by_name: HashMap<String, u64> = HashMap::new();
                    while let Some((pid, parent_created)) = stack.pop() {
                        let Some(p) = procs.get(&pid) else { continue };
                        // A process id can be reused. A real child is never older than its parent.
                        if pid != root && p.created < parent_created {
                            continue;
                        }
                        st.mem += p.private;
                        *by_name.entry(p.name.clone()).or_default() += p.private;
                        if p.name.eq_ignore_ascii_case("claude.exe") && claude.is_none_or(|(_, c)| p.created > c) {
                            claude = Some((pid, p.created));
                        }
                        if let Some(ch) = kids.get(&pid) {
                            for c in ch {
                                stack.push((*c, p.created));
                            }
                        }
                    }
                    let mut top: Vec<(String, u64)> = by_name.into_iter().collect();
                    top.sort_by(|a, b| b.1.cmp(&a.1));
                    top.truncate(4);
                    st.top = top;
                    st.claude = claude.and_then(|(pid, _)| read_session(pid));
                    snap.panes.push((pane, st));
                }
                let self_mem = procs.get(&me).map(|p| p.private).unwrap_or(0);
                snap.sys = sys_mem(self_mem);
                last = snap.clone();
                if tx.send(AppEvent::Stats(snap)).is_err() {
                    return;
                }
                wake();
                std::thread::sleep(Duration::from_millis(1000));
            }
        })
        .ok();
}

/// The chat's title: your own rename if you made one, else Claude's AI title.
/// Reads only the last 512 KB of the chat file, so it is fast even for huge chats.
pub fn chat_title(session: &str) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let dirs = std::fs::read_dir(claude_dir().join("projects")).ok()?;
    let path = dirs.flatten().map(|d| d.path().join(format!("{session}.jsonl"))).find(|p| p.exists())?;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(512 * 1024))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut ai = None;
    for line in text.lines().rev() {
        if !line.contains("-title\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("custom-title") => {
                if let Some(t) = v.get("customTitle").and_then(|t| t.as_str()) {
                    return Some(t.to_string());
                }
            }
            Some("ai-title") if ai.is_none() => {
                ai = v.get("aiTitle").or_else(|| v.get("title")).and_then(|t| t.as_str()).map(String::from);
            }
            _ => {}
        }
    }
    ai
}

/// Claude chats running in other windows (Warp, Windows Terminal...): (pid, info).
pub fn other_chats(ours: &[u32]) -> Vec<ClaudeInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(claude_dir().join("sessions")) else { return out };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(pid) = name.strip_suffix(".json").and_then(|p| p.parse::<u32>().ok()) else { continue };
        if ours.contains(&pid) || !alive(pid) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if v.get("kind").and_then(|k| k.as_str()).is_some_and(|k| k != "interactive") {
            continue;
        }
        if let Some(mut info) = read_session(pid) {
            if !transcript_exists(&info.session_id) {
                continue;
            }
            if let Some(t) = chat_title(&info.session_id) {
                info.name = t;
            }
            out.push(info);
        }
    }
    out
}

pub fn alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(h);
        ok
    }
}

/// True when Claude saved this chat to disk (an empty new chat has no file and cannot be resumed).
pub fn transcript_exists(session: &str) -> bool {
    if session.is_empty() {
        return false;
    }
    let Ok(dirs) = std::fs::read_dir(claude_dir().join("projects")) else { return false };
    let file = format!("{session}.jsonl");
    dirs.flatten().any(|d| d.path().join(&file).exists())
}

pub fn fmt_bytes(b: u64) -> String {
    let mb = b as f64 / 1_048_576.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{:.0} MB", mb)
    }
}
