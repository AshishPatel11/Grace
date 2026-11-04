//! One terminal pane: the screen grid (alacritty_terminal) + the shell process (ConPTY).

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::procmon::PaneStats;
use crate::render::RowCaches;

pub const WM_APP_WAKE: u32 = 0x8000 + 1;
static HWND_RAW: AtomicIsize = AtomicIsize::new(0);
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);

pub fn set_main_hwnd(h: HWND) {
    HWND_RAW.store(h.0 as isize, Ordering::SeqCst);
}

/// Ask the UI thread to look at new events and repaint. Many calls in a row
/// become one message, so a fast-printing program cannot flood the window.
pub fn wake() {
    if !WAKE_PENDING.swap(true, Ordering::AcqRel) {
        let h = HWND_RAW.load(Ordering::SeqCst);
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_WAKE, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn wake_handled() {
    WAKE_PENDING.store(false, Ordering::Release);
}

/// Set when a program printed something, so the screen must be redrawn.
static OUTPUT_DIRTY: AtomicBool = AtomicBool::new(false);

pub fn take_output_dirty() -> bool {
    OUTPUT_DIRTY.swap(false, Ordering::AcqRel)
}

pub enum AppEvent {
    Title(u64, Option<String>),
    Bell(u64),
    Exited(u64),
    Clipboard(String),
    Stats(crate::procmon::Snapshot),
    Ipc(crate::ipc::Request),
    /// The mic heard something (see voice.rs).
    Voice(crate::voice::VoiceEvent),
}

#[derive(Clone, Copy)]
pub struct Size {
    pub cols: usize,
    pub lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Clone)]
pub struct Proxy {
    pane: u64,
    tx: Sender<AppEvent>,
    pty: Arc<Mutex<Option<EventLoopSender>>>,
    size: Arc<Mutex<WindowSize>>,
}

impl Proxy {
    fn write(&self, bytes: Vec<u8>) {
        if let Some(tx) = self.pty.lock().unwrap().as_ref() {
            let _ = tx.send(Msg::Input(Cow::Owned(bytes)));
        }
    }
    fn send(&self, ev: AppEvent) {
        let _ = self.tx.send(ev);
        wake();
    }
}

impl EventListener for Proxy {
    fn send_event(&self, ev: Event) {
        match ev {
            Event::Wakeup => {
                OUTPUT_DIRTY.store(true, Ordering::Release);
                wake()
            }
            Event::PtyWrite(s) => self.write(s.into_bytes()),
            Event::Title(t) => self.send(AppEvent::Title(self.pane, Some(t))),
            Event::ResetTitle => self.send(AppEvent::Title(self.pane, None)),
            Event::Bell => self.send(AppEvent::Bell(self.pane)),
            Event::ChildExit(_) | Event::Exit => self.send(AppEvent::Exited(self.pane)),
            Event::ClipboardStore(_, s) => self.send(AppEvent::Clipboard(s)),
            Event::ColorRequest(i, f) => self.write(f(crate::theme::to_vrgb(crate::theme::slot(i))).into_bytes()),
            Event::TextAreaSizeRequest(f) => {
                let ws = *self.size.lock().unwrap();
                self.write(f(ws).into_bytes())
            }
            _ => {}
        }
    }
}

struct Job(HANDLE);

impl Drop for Job {
    fn drop(&mut self) {
        // Closing the last handle kills every process in the job: the shell,
        // Claude, and anything they started. No orphans left behind.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn make_job(pid: u32) -> Option<Job> {
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null()).ok()?;
        let job = Job(job);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid).ok()?;
        let ok = AssignProcessToJobObject(job.0, proc).is_ok();
        let _ = CloseHandle(proc);
        ok.then_some(job)
    }
}

pub type TermRef = Arc<FairMutex<Term<Proxy>>>;

pub struct Live {
    pub term: TermRef,
    pub tx: EventLoopSender,
    pub pid: u32,
    pub cmd: String,
    job: Option<Job>,
    size: Arc<Mutex<WindowSize>>,
}

impl Live {
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.tx.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&self, cols: usize, lines: usize, cw: f32, ch: f32) {
        let ws = WindowSize {
            num_lines: lines as u16,
            num_cols: cols as u16,
            cell_width: cw.round() as u16,
            cell_height: ch.round() as u16,
        };
        *self.size.lock().unwrap() = ws;
        self.term.lock().resize(Size { cols, lines });
        let _ = self.tx.send(Msg::Resize(ws));
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        self.job.take();
    }
}

pub struct SpawnArgs<'a> {
    pub pane: u64,
    pub cmd: &'a str,
    pub cwd: Option<&'a str>,
    pub cols: usize,
    pub lines: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    pub scrollback: usize,
    pub tx: Sender<AppEvent>,
    /// Reuse an existing screen (keeps the old output above the new shell).
    pub reuse: Option<(TermRef, Arc<Mutex<Option<EventLoopSender>>>)>,
}

pub fn spawn(a: SpawnArgs<'_>) -> std::io::Result<(Live, Arc<Mutex<Option<EventLoopSender>>>)> {
    let ws = WindowSize {
        num_lines: a.lines as u16,
        num_cols: a.cols as u16,
        cell_width: a.cell_w.round() as u16,
        cell_height: a.cell_h.round() as u16,
    };
    let size = Arc::new(Mutex::new(ws));
    let (term, pty_slot) = match a.reuse {
        Some((t, slot)) => (t, slot),
        None => {
            let slot = Arc::new(Mutex::new(None));
            let proxy = Proxy { pane: a.pane, tx: a.tx.clone(), pty: slot.clone(), size: size.clone() };
            let cfg = TermConfig { scrolling_history: a.scrollback, ..Default::default() };
            let term = Term::new(cfg, &Size { cols: a.cols, lines: a.lines }, proxy);
            (Arc::new(FairMutex::new(term)), slot)
        }
    };
    let proxy = Proxy { pane: a.pane, tx: a.tx.clone(), pty: pty_slot.clone(), size: size.clone() };

    let mut env = std::collections::HashMap::new();
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    // On Windows, Node programs (Claude Code too) only use colour and Unicode
    // symbols when they recognise the terminal. This is the name they look for.
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    env.insert("TERM_PROGRAM".to_string(), "Grace".to_string());
    env.insert("GRACE_PANE".to_string(), a.pane.to_string());
    // The old name, for scripts made before the rename.
    env.insert("PULSETERM_PANE".to_string(), a.pane.to_string());
    // If Grace was started from Warp, stop the Warp plugin from thinking it runs in Warp.
    env.insert("WARP_CLI_AGENT_PROTOCOL_VERSION".to_string(), String::new());
    env.insert("WARP_TERMINAL_SESSION_UUID".to_string(), String::new());
    let opts = tty::Options {
        shell: Some(tty::Shell::new(a.cmd.to_string(), Vec::new())),
        working_directory: a.cwd.filter(|d| std::path::Path::new(d).is_dir()).map(Into::into),
        drain_on_exit: true,
        env,
        escape_args: false,
    };
    let pty = tty::new(&opts, ws, a.pane)?;
    let pid = pty.child_watcher().pid().map(|p| p.get()).unwrap_or(0);
    let job = if pid != 0 { make_job(pid) } else { None };
    let ev_loop = EventLoop::new(term.clone(), proxy, pty, true, false)?;
    let tx = ev_loop.channel();
    *pty_slot.lock().unwrap() = Some(tx.clone());
    ev_loop.spawn();
    {
        let mut t = term.lock();
        if t.columns() != a.cols || t.screen_lines() != a.lines {
            t.resize(Size { cols: a.cols, lines: a.lines });
        }
    }
    Ok((Live { term, tx, pid, cmd: a.cmd.to_string(), job, size }, pty_slot))
}

#[derive(Clone, Debug, Default)]
pub struct Dormant {
    pub claude_session: Option<String>,
    pub claude_name: Option<String>,
    /// MB freed when the tab was put to sleep (shown on the card).
    pub freed_mb: Option<u64>,
    /// The chat is still running in another window (process id). Resuming here would run it twice.
    pub elsewhere: Option<u32>,
}

pub struct Pane {
    pub id: u64,
    pub live: Option<Live>,
    pub dormant: Option<Dormant>,
    pub profile: String,
    pub cwd: Option<String>,
    pub osc_title: Option<String>,
    pub cols: usize,
    pub lines: usize,
    pub cache: RowCaches,
    pub stats: PaneStats,
    pub prev_status: Option<String>,
    pub closing: bool,
    pub exited: bool,
    pub slot: Option<Arc<Mutex<Option<EventLoopSender>>>>,
    pub scroll_acc: f32,
    pub started: std::time::Instant,
    /// When Claude last became idle in this pane.
    pub idle_since: Option<std::time::Instant>,
    /// The typing box (only shown for Claude).
    pub compose: crate::compose::Compose,
    /// The box text measured for drawing and clicks: (text version, width, layout).
    pub compose_layout: Option<(u64, u32, windows::Win32::Graphics::DirectWrite::IDWriteTextLayout)>,
    /// Pictures pasted into Claude but not sent yet: Claude's number -> file bytes (for previews).
    pub pending_images: std::collections::HashMap<u32, Vec<u8>>,
    /// Letters drawn before Claude drew them (see `predict.rs`).
    pub predict: crate::predict::Predict,
    /// Which pictures each screen row points to, by the row's content hash (thumbnails).
    pub thumb_rows: std::collections::HashMap<u64, crate::thumbs::RowPics>,
}

impl Pane {
    /// Is Claude running here (so the typing box makes sense)?
    pub fn is_claude(&self) -> bool {
        self.live.as_ref().is_some_and(|l| self.stats.claude.is_some() || crate::config::Config::is_claude_command(&l.cmd))
    }

    pub fn new(id: u64, profile: &str, cwd: Option<String>) -> Pane {
        Pane {
            id,
            live: None,
            dormant: None,
            profile: profile.to_string(),
            cwd,
            osc_title: None,
            cols: 80,
            lines: 24,
            cache: Default::default(),
            stats: PaneStats::default(),
            prev_status: None,
            closing: false,
            exited: false,
            slot: None,
            scroll_acc: 0.0,
            started: std::time::Instant::now(),
            idle_since: None,
            compose: Default::default(),
            compose_layout: None,
            pending_images: Default::default(),
            predict: Default::default(),
            thumb_rows: Default::default(),
        }
    }

    pub fn claude_status(&self) -> Option<&str> {
        self.stats.claude.as_ref().map(|c| c.status.as_str())
    }

    /// Best name for the tab: Claude's chat name, then the program's title.
    pub fn display_title(&self) -> String {
        if let Some(d) = &self.dormant {
            if let Some(n) = &d.claude_name {
                return n.clone();
            }
        }
        // The program's own title first: Claude sets it to the chat topic.
        if let Some(t) = &self.osc_title {
            let t = t.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '\\' && c != '/' && c != '.').trim();
            if t.to_ascii_lowercase().ends_with(".exe") {
                // Shells put their own path in the title; the profile name reads better.
                return self.profile.clone();
            }
            let generic = t.eq_ignore_ascii_case("claude code") || t.eq_ignore_ascii_case("claude");
            if !t.is_empty() && !(generic && self.stats.claude.as_ref().is_some_and(|c| !c.name.is_empty())) {
                return t.to_string();
            }
        }
        if let Some(c) = &self.stats.claude {
            if !c.name.is_empty() {
                return c.name.clone();
            }
        }
        self.profile.clone()
    }
}
