//! The app: tabs, panes, keyboard, mouse, alerts, sleep/resume, saved tabs.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::index::Direction;
use serde_json::json;
use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowPlacement, SetWindowTextW, HICON, SW_SHOWMAXIMIZED,
    WINDOWPLACEMENT,
};

use crate::config::{self, Config, SavedNode, SavedPane, SavedSession, SavedTab};
use crate::input::*;
use crate::layout::{neighbor, Divider, Node, Rect};
use crate::links::{self, Link, Target};
use crate::pane::{spawn, AppEvent, Dormant, Pane, SpawnArgs};
use crate::procmon::{self, SysMem};
use crate::win;

pub const BAR_H: f32 = 38.0;
pub const PAD_X: f32 = 10.0;
pub const PAD_Y: f32 = 6.0;
pub const GAP: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alert {
    None,
    Done,
    Asking,
}

pub struct Tab {
    pub id: u64,
    pub root: Node,
    pub focus: u64,
    pub custom_title: Option<String>,
    pub alert: Alert,
    /// One pane shown full size (Ctrl+Shift+Z).
    pub zoom: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct TabHit {
    pub rect: Rect,
    pub close: Rect,
    pub badge: Rect,
}

#[derive(Clone, Debug, Default)]
pub enum Drag {
    #[default]
    None,
    Select(u64),
    Divider(Divider),
    Tab { idx: usize, start_x: f32, moved: bool },
    Scroll(u64),
    /// Selecting text in a typing box.
    Compose(u64),
    /// Button down in a program that uses the mouse (Claude): a drag will select text
    /// here, a plain click goes to the program when the button comes up.
    Pending { id: u64, x: f32, y: f32 },
    /// The program gets this press, its moves and its release.
    App(u64),
}

pub struct Search {
    pub pane: u64,
    pub query: String,
    pub found: Option<bool>,
}

#[derive(Default)]
pub struct Ui {
    pub tabs: Vec<TabHit>,
    pub plus: Rect,
    pub menu: Rect,
    pub sys: Rect,
    pub panes: Vec<(u64, Rect)>,
    pub dividers: Vec<Divider>,
    pub mouse: (f32, f32),
    pub drag: Drag,
    pub rename: Option<(usize, String)>,
    pub search: Option<Search>,
    pub clicks: (u32, f32, f32, u32),
    pub link: Option<(u64, Link)>,
    pub jump: Vec<(u64, Rect)>,
    pub cards: Vec<(u64, Rect)>,
    pub tracking: bool,
    /// Typing boxes in the active tab.
    pub boxes: Vec<crate::composebox::BoxHit>,
    /// The picture under the mouse, shown as a preview.
    pub preview: Option<crate::preview::Preview>,
    /// Toolbars (mic, Rich input) under Claude tabs in the active tab.
    pub tools: Vec<crate::toolbar::ToolHit>,
    /// The strip at the right of a Claude pane where thumbnails go.
    pub gutters: Vec<(u64, Rect)>,
    /// Thumbnails drawn in the last frame (click one to open it big).
    pub thumbs: Vec<(Rect, crate::preview::Preview)>,
    /// A picture opened big, over everything. Esc or a click closes it.
    pub modal: Option<crate::preview::Preview>,
    /// The recent folders shown in the last folder menu (menu id 200 + index).
    pub folder_menu: Vec<String>,
}

pub enum Deferred {
    Menu { items: Vec<(u32, String)>, x: i32, y: i32, ctx: MenuCtx },
    Confirm { text: String, then: Confirmed },
    /// Windows' folder picker, then Claude opens in the chosen folder.
    PickFolder { start: Option<String> },
}

#[derive(Clone, Copy)]
pub enum MenuCtx {
    Tab(usize),
    Pane(u64),
    Profiles,
    /// The folder button under a Claude pane.
    Folder(u64),
}

pub enum Confirmed {
    ClosePane(u64),
    CloseTab(usize),
    CloseWindow,
    Paste(u64, String),
    Refresh(usize),
    ResumeAnyway(u64),
}

pub struct App {
    pub hwnd: HWND,
    pub cfg: Config,
    pub gfx: crate::gfx::Gfx,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub panes: HashMap<u64, Pane>,
    next_id: u64,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    targets: Arc<Mutex<Vec<(u64, u32)>>>,
    pub sys: SysMem,
    pub ui: Ui,
    pub focused: bool,
    pub cursor_on: bool,
    pub pending: Vec<Deferred>,
    pub icon: HICON,
    high_surrogate: Option<u16>,
    last_title: String,
    alert_tab: Option<u64>,
    pub quit_ok: bool,
    pub toast: Option<(String, Instant)>,
    /// Time of the last frames, in microseconds (shown by `list`, for checking speed).
    pub frame_us: std::collections::VecDeque<u64>,
    /// Since last `list`: lock wait, copy, draw (microseconds), rows shaped.
    pub perf: (u64, u64, u64, u64),
    /// Where the pictures of each chat are in its log (for previews).
    pub chat_images: HashMap<String, crate::images::ChatImages>,
    /// Thumbnails that could not be loaded, and when we last tried (retry after a while).
    pub thumb_miss: HashMap<String, Instant>,
}

fn key(vk: u16) -> bool {
    unsafe { GetKeyState(vk as i32) < 0 }
}

impl App {
    pub fn new(hwnd: HWND, cfg: Config, scale: f32, icon: HICON) -> windows::core::Result<App> {
        let gfx = crate::gfx::Gfx::new(&cfg.font_family, cfg.font_size, scale)?;
        let (tx, rx) = channel();
        let targets = Arc::new(Mutex::new(Vec::new()));
        procmon::start(targets.clone(), tx.clone());
        crate::ipc::start_server(tx.clone());
        Ok(App {
            hwnd,
            cfg,
            gfx,
            tabs: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            next_id: 1,
            tx,
            rx,
            targets,
            sys: SysMem::default(),
            ui: Ui::default(),
            focused: true,
            cursor_on: true,
            pending: Vec::new(),
            icon,
            high_surrogate: None,
            last_title: String::new(),
            alert_tab: None,
            quit_ok: false,
            toast: None,
            frame_us: Default::default(),
            perf: (0, 0, 0, 0),
            chat_images: HashMap::new(),
            thumb_miss: HashMap::new(),
        })
    }

    pub fn s(&self, v: f32) -> f32 {
        (v * self.gfx.scale).round()
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn client(&self) -> (f32, f32) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        (rc.right as f32, rc.bottom as f32)
    }

    // ---------------- startup ----------------

    pub fn restore_or_new(&mut self, new_if_empty: bool) {
        let saved = if self.cfg.restore_tabs { config::load_session() } else { None };
        if let Some(s) = saved.filter(|s| !s.tabs.is_empty()) {
            for t in &s.tabs {
                let root = self.build_saved(&t.layout);
                let focus = root.leaves()[0];
                let id = self.id();
                self.tabs.push(Tab { id, root, focus, custom_title: t.title.clone(), alert: Alert::None, zoom: None });
            }
            self.active = s.active.min(self.tabs.len() - 1);
            self.relayout();
            self.wake_tab(self.active);
        } else if new_if_empty {
            let p = self.cfg.default_profile.clone();
            self.new_tab(&p, None);
        }
    }

    fn build_saved(&mut self, n: &SavedNode) -> Node {
        match n {
            SavedNode::Leaf(sp) => {
                let id = self.id();
                let mut p = Pane::new(id, &sp.profile, sp.cwd.clone());
                p.dormant = Some(Dormant {
                    claude_session: sp.claude_session.clone().filter(|s| procmon::transcript_exists(s)),
                    claude_name: sp.claude_name.clone(),
                    freed_mb: None,
                    elsewhere: None,
                });
                self.panes.insert(id, p);
                Node::Leaf(id)
            }
            SavedNode::Split { vertical, ratio, a, b } => Node::Split {
                vertical: *vertical,
                ratio: *ratio,
                a: Box::new(self.build_saved(a)),
                b: Box::new(self.build_saved(b)),
            },
        }
    }

    pub fn saved_window() -> Option<config::SavedWindow> {
        config::load_session().and_then(|s| s.window)
    }

    pub fn save_session(&self) {
        let mut tabs = Vec::new();
        for t in &self.tabs {
            tabs.push(SavedTab { title: t.custom_title.clone(), layout: self.save_node(&t.root) });
        }
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        let window = unsafe { GetWindowPlacement(self.hwnd, &mut wp) }.ok().map(|_| {
            let r = wp.rcNormalPosition;
            config::SavedWindow {
                x: r.left,
                y: r.top,
                w: r.right - r.left,
                h: r.bottom - r.top,
                maximized: wp.showCmd == SW_SHOWMAXIMIZED.0 as u32,
            }
        });
        config::save_session(&SavedSession { window, tabs, active: self.active });
    }

    fn save_node(&self, n: &Node) -> SavedNode {
        match n {
            Node::Leaf(id) => {
                let p = &self.panes[id];
                let (session, name) = match (&p.dormant, &p.stats.claude) {
                    (Some(d), _) => (d.claude_session.clone(), d.claude_name.clone()),
                    (None, Some(c)) => (Some(c.session_id.clone()), Some(p.display_title())),
                    _ => (None, None),
                };
                let session = session.filter(|s| procmon::transcript_exists(s));
                let profile = if session.is_some() { "Claude".to_string() } else { p.profile.clone() };
                SavedNode::Leaf(SavedPane { profile, cwd: p.cwd.clone(), claude_session: session, claude_name: name })
            }
            Node::Split { vertical, ratio, a, b } => SavedNode::Split {
                vertical: *vertical,
                ratio: *ratio,
                a: Box::new(self.save_node(a)),
                b: Box::new(self.save_node(b)),
            },
        }
    }

    // ---------------- panes ----------------

    fn cmd_for(&self, p: &Pane) -> (String, Option<String>) {
        if let Some(sid) = p.dormant.as_ref().and_then(|d| d.claude_session.clone()) {
            return (format!("{} --resume {}", self.cfg.claude_command, sid), p.cwd.clone());
        }
        let prof = self.cfg.profile(&p.profile);
        (prof.command, p.cwd.clone().or(prof.cwd))
    }

    fn start_pane(&mut self, id: u64, cmd_override: Option<String>, reuse: bool) {
        let (cols, lines, cw, ch) = {
            let p = &self.panes[&id];
            (p.cols, p.lines, self.gfx.cell_w, self.gfx.cell_h)
        };
        let (mut cmd, cwd) = self.cmd_for(&self.panes[&id]);
        if let Some(c) = cmd_override {
            cmd = c;
        }
        let reuse_args = if reuse {
            let p = self.panes.get_mut(&id).unwrap();
            match (p.live.take(), p.slot.clone()) {
                (Some(old), Some(slot)) => {
                    let t = old.term.clone();
                    std::mem::forget(old); // the old process already ended; keep its screen
                    Some((t, slot))
                }
                _ => None,
            }
        } else {
            None
        };
        let res = spawn(SpawnArgs {
            pane: id,
            cmd: &cmd,
            cwd: cwd.as_deref(),
            cols,
            lines,
            cell_w: cw,
            cell_h: ch,
            scrollback: self.cfg.scrollback_lines,
            tx: self.tx.clone(),
            reuse: reuse_args,
        });
        let p = self.panes.get_mut(&id).unwrap();
        match res {
            Ok((live, slot)) => {
                let pid = live.pid;
                p.live = Some(live);
                p.slot = Some(slot);
                p.dormant = None;
                p.exited = false;
                p.started = Instant::now();
                p.cache.clear();
                let mut t = self.targets.lock().unwrap();
                t.retain(|(pid_pane, _)| *pid_pane != id);
                t.push((id, pid));
            }
            Err(e) => {
                crate::log(&format!("could not start '{cmd}': {e}"));
                p.dormant = Some(Dormant { claude_name: Some(format!("Could not start: {cmd}")), ..Default::default() });
            }
        }
    }

    fn add_pane(&mut self, profile: &str, cwd: Option<String>) -> u64 {
        let id = self.id();
        self.panes.insert(id, Pane::new(id, profile, cwd));
        id
    }

    pub fn new_tab(&mut self, profile: &str, cwd: Option<String>) {
        // A folder asked for by name always wins (Duplicate tab, "Open Claude in folder").
        let asked = cwd.is_some();
        let cwd = cwd.or_else(|| self.focused_pane().and_then(|p| p.cwd.clone()));
        let prof = self.cfg.profile(profile);
        let cwd = if !asked && prof.cwd.is_some() && profile != self.focused_pane().map(|p| p.profile.clone()).unwrap_or_default() {
            prof.cwd.clone()
        } else {
            cwd.or(prof.cwd.clone())
        };
        let id = self.add_pane(&prof.name, cwd);
        let tid = self.id();
        self.tabs.push(Tab { id: tid, root: Node::Leaf(id), focus: id, custom_title: None, alert: Alert::None, zoom: None });
        self.active = self.tabs.len() - 1;
        self.relayout();
        self.start_pane(id, None, false);
        self.invalidate();
    }

    pub fn split(&mut self, vertical: bool) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let target = tab.focus;
        let (profile, cwd) = {
            let p = &self.panes[&target];
            (p.profile.clone(), p.cwd.clone())
        };
        let id = self.add_pane(&profile, cwd);
        let tab = &mut self.tabs[self.active];
        tab.root.split(target, id, vertical);
        tab.focus = id;
        self.relayout();
        self.start_pane(id, None, false);
        self.invalidate();
    }

    pub fn focused_id(&self) -> Option<u64> {
        self.tabs.get(self.active).map(|t| t.focus)
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.focused_id().and_then(|id| self.panes.get(&id))
    }

    fn tab_of(&self, pane: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.root.leaves().contains(&pane))
    }

    fn kill_pane(&mut self, id: u64) {
        if let Some(mut p) = self.panes.remove(&id) {
            p.closing = true;
            p.live = None;
        }
        self.targets.lock().unwrap().retain(|(p, _)| *p != id);
    }

    pub fn close_pane(&mut self, id: u64, ask: bool) {
        if ask && self.panes.get(&id).and_then(|p| p.claude_status()) == Some("busy") {
            self.pending.push(Deferred::Confirm {
                text: "Claude is still working in this pane. Close it anyway?".into(),
                then: Confirmed::ClosePane(id),
            });
            return;
        }
        let Some(ti) = self.tab_of(id) else { return };
        if !self.tabs[ti].root.remove(id) {
            self.close_tab(ti, false);
            return;
        }
        self.kill_pane(id);
        let tab = &mut self.tabs[ti];
        if tab.focus == id {
            tab.focus = tab.root.leaves()[0];
        }
        self.relayout();
        self.invalidate();
    }

    pub fn close_tab(&mut self, idx: usize, ask: bool) {
        if idx >= self.tabs.len() {
            return;
        }
        let leaves = self.tabs[idx].root.leaves();
        if ask && leaves.iter().any(|id| self.panes.get(id).and_then(|p| p.claude_status()) == Some("busy")) {
            self.pending.push(Deferred::Confirm {
                text: "Claude is still working in this tab. Close it anyway?".into(),
                then: Confirmed::CloseTab(idx),
            });
            return;
        }
        for id in leaves {
            self.kill_pane(id);
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.quit_ok = true;
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(self.hwnd),
                    windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    Default::default(),
                    Default::default(),
                );
            }
            return;
        }
        if self.active >= self.tabs.len() || self.active > idx {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.activate(self.active);
    }

    pub fn activate(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.active = idx;
        self.ui.search = None;
        self.ui.rename = None;
        if self.focused {
            self.tabs[idx].alert = Alert::None;
        }
        self.relayout();
        self.wake_tab(idx);
        self.update_title();
        self.invalidate();
    }

    /// Start the plain shells of a restored tab. Chats wait for Enter (they can use a lot of RAM).
    fn wake_tab(&mut self, idx: usize) {
        for id in self.tabs[idx].root.leaves() {
            let wake = self.panes.get(&id).is_some_and(|p| {
                p.dormant.as_ref().is_some_and(|d| d.claude_session.is_none() && d.freed_mb.is_none())
                    && !Config::is_claude_command(&self.cfg.profile(&p.profile).command)
            });
            if wake {
                self.start_pane(id, None, false);
            }
        }
    }

    /// Open a new tab that resumes an existing chat (`pulseterm --resume <id>`).
    pub fn resume_chat(&mut self, session: &str, cwd: Option<String>) {
        let id = self.add_pane("Claude", cwd.or_else(|| self.cfg.profile("Claude").cwd));
        if let Some(p) = self.panes.get_mut(&id) {
            p.dormant = Some(Dormant { claude_session: Some(session.to_string()), ..Default::default() });
        }
        let tid = self.id();
        self.tabs.push(Tab { id: tid, root: Node::Leaf(id), focus: id, custom_title: None, alert: Alert::None, zoom: None });
        self.active = self.tabs.len() - 1;
        self.relayout();
        self.start_pane(id, None, false);
        self.invalidate();
    }

    pub fn resume(&mut self, id: u64) {
        let other = self.panes.get(&id).and_then(|p| p.dormant.as_ref()).and_then(|d| d.elsewhere);
        if let Some(pid) = other.filter(|pid| procmon::alive(*pid)) {
            self.pending.push(Deferred::Confirm {
                text: format!(
                    "This chat is still open in another window (process {pid}).\n\nClose it there first. If you resume it here too, two copies of the same chat run at once.\n\nResume here anyway?"
                ),
                then: Confirmed::ResumeAnyway(id),
            });
            return;
        }
        if self.panes.get(&id).is_some_and(|p| p.dormant.is_some()) {
            self.start_pane(id, None, false);
            self.invalidate();
        }
    }

    /// Restart Claude in this tab with the same chat. A long-running Claude keeps growing;
    /// a fresh one loads the same chat and uses far less (measured 1,655 MB -> 580 MB).
    pub fn refresh_tab(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else { return };
        for id in tab.root.leaves() {
            let Some(p) = self.panes.get_mut(&id) else { continue };
            let Some(c) = p.stats.claude.clone() else { continue };
            if !procmon::transcript_exists(&c.session_id) {
                continue;
            }
            let name = p.display_title();
            p.closing = true;
            p.live = None;
            p.closing = false;
            p.stats = Default::default();
            p.prev_status = None;
            p.idle_since = None;
            p.dormant = Some(Dormant { claude_session: Some(c.session_id), claude_name: Some(name), ..Default::default() });
            self.targets.lock().unwrap().retain(|(x, _)| *x != id);
            self.start_pane(id, None, false);
        }
        self.invalidate();
    }

    /// Claude chats open in other windows (Warp...) become sleeping tabs here.
    pub fn import_chats(&mut self) -> usize {
        let ours: Vec<u32> = self.panes.values().filter_map(|p| p.stats.claude.as_ref().map(|c| c.pid)).collect();
        let known: Vec<String> = self
            .panes
            .values()
            .filter_map(|p| {
                p.stats
                    .claude
                    .as_ref()
                    .map(|c| c.session_id.clone())
                    .or_else(|| p.dormant.as_ref().and_then(|d| d.claude_session.clone()))
            })
            .collect();
        let mut n = 0;
        for c in procmon::other_chats(&ours) {
            if known.contains(&c.session_id) {
                continue;
            }
            let id = self.add_pane("Claude", Some(c.cwd.clone()).filter(|s| !s.is_empty()));
            if let Some(p) = self.panes.get_mut(&id) {
                p.dormant = Some(Dormant {
                    claude_session: Some(c.session_id.clone()),
                    claude_name: Some(if c.name.is_empty() { "Imported chat".into() } else { c.name.clone() }),
                    freed_mb: None,
                    elsewhere: Some(c.pid),
                });
            }
            let tid = self.id();
            self.tabs.push(Tab { id: tid, root: Node::Leaf(id), focus: id, custom_title: None, alert: Alert::None, zoom: None });
            n += 1;
        }
        self.relayout();
        self.invalidate();
        n
    }

    /// A short message at the bottom of the window. It goes away after 4 seconds.
    pub fn show_toast(&mut self, text: String) {
        self.toast = Some((text, Instant::now()));
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(self.hwnd), 2, 4000, None);
        }
        self.invalidate();
    }

    pub fn toggle_zoom(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        tab.zoom = if tab.zoom.is_some() { None } else { Some(tab.focus) };
        self.relayout();
        self.invalidate();
    }

    /// True when a Claude in this tab is big and has been idle 10+ minutes.
    pub fn refresh_hint(&self, idx: usize) -> bool {
        let warn = self.cfg.ram_warn_mb * 1_048_576;
        self.tabs[idx].root.leaves().iter().filter_map(|id| self.panes.get(id)).any(|p| {
            p.stats.claude.is_some() && p.stats.mem >= warn && p.idle_since.is_some_and(|t| t.elapsed() >= Duration::from_secs(600))
        })
    }

    /// Estimated RAM a refresh gives back: a fresh Claude with a big chat used ~600 MB here.
    pub fn refresh_gain(&self, idx: usize) -> u64 {
        self.tabs[idx]
            .root
            .leaves()
            .iter()
            .filter_map(|id| self.panes.get(id))
            .filter(|p| p.stats.claude.is_some())
            .map(|p| p.stats.mem.saturating_sub(600 * 1_048_576))
            .sum()
    }

    /// Stop a tab's programs to free RAM. The chat id is kept, so Enter resumes it.
    pub fn sleep_tab(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else { return };
        for id in tab.root.leaves() {
            let Some(p) = self.panes.get_mut(&id) else { continue };
            if p.live.is_none() {
                continue;
            }
            let freed = p.stats.mem / 1_048_576;
            let claude = p.stats.claude.clone();
            let name = p.display_title();
            p.closing = true;
            p.live = None;
            p.closing = false;
            p.cache.clear();
            p.dormant = Some(Dormant {
                claude_session: claude
                    .as_ref()
                    .map(|c| c.session_id.clone())
                    .filter(|s| procmon::transcript_exists(s)),
                claude_name: Some(name),
                freed_mb: Some(freed),
                elsewhere: None,
            });
            p.stats = Default::default();
            p.prev_status = None;
            self.targets.lock().unwrap().retain(|(x, _)| *x != id);
        }
        self.tabs[idx].alert = Alert::None;
        self.invalidate();
    }

    // ---------------- layout ----------------

    pub fn bar_h(&self) -> f32 {
        self.s(BAR_H)
    }

    pub fn relayout(&mut self) {
        let (w, h) = self.client();
        let top = self.bar_h();
        if w < 50.0 || h < top + 20.0 {
            // Minimized or not shown yet: keep the old sizes.
            return;
        }
        let area = Rect::new(0.0, top, w, (h - top).max(0.0));
        let gap = self.s(GAP);
        let (px, py) = (self.s(PAD_X), self.s(PAD_Y));
        let (cw, ch) = (self.gfx.cell_w, self.gfx.cell_h);
        let mut all = Vec::new();
        for tab in &self.tabs {
            let mut rects = Vec::new();
            let mut divs = Vec::new();
            match tab.zoom.filter(|z| tab.root.leaves().contains(z)) {
                Some(z) => rects.push((z, area)),
                None => tab.root.layout(area, gap, &mut rects, &mut divs),
            }
            all.push((rects, divs));
        }
        for (ti, (rects, divs)) in all.into_iter().enumerate() {
            let mut term_rects = Vec::new();
            let mut boxes = Vec::new();
            let mut tools = Vec::new();
            let mut gutters = Vec::new();
            for (id, r) in rects {
                // A Claude pane gets, from the bottom up: the toolbar, the typing box (when
                // open), and the terminal. Thumbnails get a strip at the terminal's right.
                let mut tr = r;
                let bar = self.toolbar_wanted(id);
                if bar {
                    let t = self.toolbar_geometry(id, r);
                    tr = Rect::new(r.x, r.y, r.w, (t.rect.y - r.y).max(ch + 2.0 * py));
                    tools.push(t);
                }
                if let Some(p) = self.panes.get_mut(&id) {
                    p.compose.bar = bar;
                }
                if self.box_wanted(id) {
                    let area = tr;
                    let (b, lines) = self.box_geometry(id, area);
                    tr = Rect::new(r.x, r.y, r.w, (b.rect.y - r.y).max(ch + 2.0 * py));
                    if let Some(p) = self.panes.get_mut(&id) {
                        p.compose.shown = true;
                        p.compose.shown_lines = lines;
                    }
                    boxes.push(b);
                } else if let Some(p) = self.panes.get_mut(&id) {
                    p.compose.shown = false;
                }
                // Thumbnail strip: only for Claude, and only when the pane is wide enough
                // that losing a few columns does not hurt.
                let gw = self.thumb_gutter_w();
                if bar && self.cfg.thumbnails && tr.w - gw > 90.0 * cw {
                    gutters.push((id, Rect::new(tr.right() - gw, tr.y, gw, tr.h)));
                    tr = Rect::new(tr.x, tr.y, tr.w - gw, tr.h);
                }
                if let Some(p) = self.panes.get_mut(&id) {
                    let cols = (((tr.w - 2.0 * px) / cw).floor() as usize).max(2);
                    let lines = (((tr.h - 2.0 * py) / ch).floor() as usize).max(1);
                    if cols != p.cols || lines != p.lines {
                        p.cols = cols;
                        p.lines = lines;
                        p.cache.clear();
                        if let Some(l) = &p.live {
                            l.resize(cols, lines, cw, ch);
                        }
                    }
                }
                term_rects.push((id, tr));
            }
            if ti == self.active {
                self.ui.panes = term_rects;
                self.ui.dividers = divs;
                self.ui.boxes = boxes;
                self.ui.tools = tools;
                self.ui.gutters = gutters;
            }
        }
        self.layout_bar();
    }

    pub fn layout_bar(&mut self) {
        let (w, _) = self.client();
        let s = self.gfx.scale;
        let top = (6.0 * s).round();
        let h = self.bar_h() - top;
        let sys_w = (150.0 * s).round();
        let btn = (28.0 * s).round();
        let left = (8.0 * s).round();
        let avail = (w - left - sys_w - 2.0 * btn - 16.0 * s).max(100.0);
        let n = self.tabs.len().max(1) as f32;
        let tw = (avail / n).clamp(90.0 * s, 230.0 * s).floor();
        let mut x = left;
        self.ui.tabs.clear();
        for _ in &self.tabs {
            let r = Rect::new(x, top, tw, h);
            let cs = (18.0 * s).round();
            let close = Rect::new(r.right() - cs - 8.0 * s, r.y + (h - cs) / 2.0, cs, cs);
            let bw = (44.0 * s).round();
            let badge = Rect::new(close.x - bw - 4.0 * s, r.y + (h - 16.0 * s) / 2.0, bw, (16.0 * s).round());
            self.ui.tabs.push(TabHit { rect: r, close, badge });
            x += tw + (2.0 * s).round();
        }
        let by = top + (h - btn) / 2.0;
        self.ui.plus = Rect::new(x + 4.0 * s, by, btn, btn);
        self.ui.menu = Rect::new(self.ui.plus.right(), by, (18.0 * s).round(), btn);
        self.ui.sys = Rect::new(w - sys_w - 8.0 * s, top, sys_w, h);
    }

    pub fn text_origin(&self, r: Rect) -> (f32, f32) {
        (r.x + self.s(PAD_X), r.y + self.s(PAD_Y))
    }

    pub fn set_scale(&mut self, scale: f32) {
        let fam = self.cfg.font_family.clone();
        let pt = self.gfx.font_pt;
        let _ = self.gfx.set_font(&fam, pt, scale);
        for p in self.panes.values_mut() {
            p.cache.clear();
        }
        self.relayout();
        self.invalidate();
    }

    fn zoom(&mut self, delta: f32) {
        let fam = self.cfg.font_family.clone();
        let pt = if delta == 0.0 { self.cfg.font_size } else { (self.gfx.font_pt + delta).clamp(6.0, 40.0) };
        let scale = self.gfx.scale;
        let _ = self.gfx.set_font(&fam, pt, scale);
        for p in self.panes.values_mut() {
            p.cache.clear();
        }
        self.relayout();
        self.invalidate();
    }

    // ---------------- titles & alerts ----------------

    pub fn tab_title(&self, idx: usize) -> String {
        let t = &self.tabs[idx];
        if let Some(c) = &t.custom_title {
            return c.clone();
        }
        self.panes.get(&t.focus).map(|p| p.display_title()).unwrap_or_default()
    }

    /// "busy", "asking", "idle", "shell" (no Claude), "sleeping", "exited".
    pub fn tab_status(&self, idx: usize) -> &'static str {
        let leaves = self.tabs[idx].root.leaves();
        let ps: Vec<&Pane> = leaves.iter().filter_map(|id| self.panes.get(id)).collect();
        if ps.iter().any(|p| p.claude_status() == Some("asking")) {
            return "asking";
        }
        if ps.iter().any(|p| p.claude_status() == Some("busy")) {
            return "busy";
        }
        if ps.iter().any(|p| p.stats.claude.is_some()) {
            return "idle";
        }
        if !ps.is_empty() && ps.iter().all(|p| p.dormant.is_some()) {
            return "sleeping";
        }
        "shell"
    }

    pub fn tab_mem(&self, idx: usize) -> u64 {
        self.tabs[idx].root.leaves().iter().filter_map(|id| self.panes.get(id)).map(|p| p.stats.mem).sum()
    }

    fn update_title(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let name = if crate::dev() { "PulseTerm (test copy)" } else { "PulseTerm" };
        let t = format!("{} — {name}", self.tab_title(self.active));
        if t != self.last_title {
            unsafe {
                let _ = SetWindowTextW(self.hwnd, &HSTRING::from(t.as_str()));
            }
            self.last_title = t;
        }
    }

    fn alert(&mut self, ti: usize, kind: Alert, body: String) {
        let visible = ti == self.active && self.focused;
        if visible {
            return;
        }
        self.tabs[ti].alert = kind;
        if !self.focused {
            win::flash(self.hwnd);
            if self.cfg.notify_popup {
                let title = if kind == Alert::Asking { "Claude needs you" } else { "Claude finished" };
                win::tray_popup(self.hwnd, title, &body, kind == Alert::Asking);
                self.alert_tab = Some(self.tabs[ti].id);
            }
        }
    }

    pub fn on_tray(&mut self) {
        if let Some(tid) = self.alert_tab.take() {
            if let Some(i) = self.tabs.iter().position(|t| t.id == tid) {
                self.activate(i);
            }
        }
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::*;
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    // ---------------- events from other threads ----------------

    /// What the tab strip shows. If this did not change, a stats update needs no repaint.
    fn bar_signature(&self) -> String {
        let mut s = String::new();
        for i in 0..self.tabs.len() {
            s.push_str(&format!(
                "{}|{}|{:?}|{};",
                self.tab_status(i),
                procmon::fmt_bytes(self.tab_mem(i)),
                self.tabs[i].alert,
                self.tab_title(i)
            ));
        }
        let gb = |b: u64| (b as f64 / 1.073_741_824e8).round() as u64;
        s.push_str(&format!("{}/{}", gb(self.sys.total - self.sys.avail), gb(self.sys.total)));
        s
    }

    pub fn cursor_visible(&self) -> bool {
        if self.box_takes_keys().is_some() {
            return true;
        }
        self.focused_pane().and_then(|p| p.live.as_ref()).is_some_and(|l| {
            let t = l.term.lock();
            t.mode().contains(TermMode::SHOW_CURSOR) && t.grid().display_offset() == 0
        })
    }

    pub fn drain(&mut self) {
        crate::pane::wake_handled();
        let mut paint = crate::pane::take_output_dirty();
        let before = self.bar_signature();
        while let Ok(ev) = self.rx.try_recv() {
            if !matches!(ev, AppEvent::Stats(_)) {
                paint = true;
            }
            match ev {
                AppEvent::Title(id, t) => {
                    if let Some(p) = self.panes.get_mut(&id) {
                        p.osc_title = t;
                    }
                }
                AppEvent::Bell(id) => {
                    if let Some(ti) = self.tab_of(id) {
                        if !(ti == self.active && self.focused) {
                            win::flash(self.hwnd);
                        }
                    }
                }
                AppEvent::Exited(id) => self.on_exit(id),
                AppEvent::Clipboard(s) => win::set_clipboard_text(self.hwnd, &s),
                AppEvent::Stats(snap) => self.on_stats(snap),
                AppEvent::Ipc(req) => {
                    let out = self.on_ipc(&req.json);
                    let _ = req.reply.send(out.to_string());
                }
            }
        }
        self.update_title();
        self.box_check();
        if paint || self.bar_signature() != before {
            self.invalidate();
        }
    }

    fn on_exit(&mut self, id: u64) {
        let Some(p) = self.panes.get_mut(&id) else { return };
        if p.closing || p.live.is_none() {
            return;
        }
        let was_claude = p.live.as_ref().is_some_and(|l| Config::is_claude_command(&l.cmd));
        p.exited = true;
        self.targets.lock().unwrap().retain(|(x, _)| *x != id);
        if was_claude && p.started.elapsed() > Duration::from_secs(2) {
            // Like Warp: when the chat ends you land in a shell in the same folder.
            if let Some(c) = &p.stats.claude {
                if !c.cwd.is_empty() {
                    p.cwd = Some(c.cwd.clone());
                }
            }
            p.stats = Default::default();
            p.prev_status = None;
            let shell = self.cfg.fallback_shell.clone();
            self.start_pane(id, Some(shell), true);
            return;
        }
        if p.started.elapsed() < Duration::from_secs(2) {
            // It died at once: keep the pane so the error stays readable.
            return;
        }
        self.close_pane(id, false);
    }

    fn on_stats(&mut self, snap: procmon::Snapshot) {
        self.sys = snap.sys;
        let mut alerts = Vec::new();
        for (id, mut st) in snap.panes {
            let Some(p) = self.panes.get_mut(&id) else { continue };
            if p.live.is_none() {
                continue;
            }
            if let Some(c) = &mut st.claude {
                if c.status == "shell" {
                    c.status = "idle".into();
                }
                if !c.cwd.is_empty() {
                    p.cwd = Some(c.cwd.clone());
                }
            }
            let new = st.claude.as_ref().map(|c| c.status.clone());
            let old = p.prev_status.take();
            if new.as_deref() == Some("idle") {
                if old.as_deref() != Some("idle") || p.idle_since.is_none() {
                    p.idle_since = Some(Instant::now());
                }
            } else {
                p.idle_since = None;
            }
            p.prev_status = new.clone();
            let waiting = st.claude.as_ref().map(|c| c.waiting_for.clone()).unwrap_or_default();
            p.stats = st;
            let name = p.display_title();
            match (old.as_deref(), new.as_deref()) {
                (Some("busy"), Some("idle")) => alerts.push((id, Alert::Done, format!("{name} is done."))),
                (o, Some("asking")) if o != Some("asking") => {
                    let what = if waiting.is_empty() { "your answer".to_string() } else { waiting };
                    alerts.push((id, Alert::Asking, format!("{name} is waiting for {what}.")))
                }
                _ => {}
            }
        }
        for (id, kind, body) in alerts {
            if let Some(ti) = self.tab_of(id) {
                self.alert(ti, kind, body);
            }
        }
        for (ti, t) in self.tabs.iter_mut().enumerate() {
            if t.alert == Alert::Asking {
                let still = t.root.leaves().iter().any(|id| self.panes.get(id).and_then(|p| p.claude_status()) == Some("asking"));
                if !still {
                    t.alert = Alert::None;
                }
            }
            if ti == self.active && self.focused {
                t.alert = Alert::None;
            }
        }
    }

    // ---------------- IPC ----------------

    fn tab_arg(&self, j: &serde_json::Value) -> Option<usize> {
        match j.get("tab").and_then(|v| v.as_u64()) {
            Some(n) if n >= 1 => Some(n as usize - 1).filter(|i| *i < self.tabs.len()),
            Some(_) => None,
            None => Some(self.active),
        }
    }

    fn on_ipc(&mut self, j: &serde_json::Value) -> serde_json::Value {
        let cmd = j.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
        match cmd {
            "list" => {
                let tabs: Vec<_> = (0..self.tabs.len())
                    .map(|i| {
                        let session = self.tabs[i].root.leaves().iter().find_map(|id| {
                            let p = self.panes.get(id)?;
                            p.stats
                                .claude
                                .as_ref()
                                .map(|c| c.session_id.clone())
                                .or_else(|| p.dormant.as_ref().and_then(|d| d.claude_session.clone()))
                        });
                        json!({
                            "index": i + 1,
                            "title": self.tab_title(i),
                            "status": self.tab_status(i),
                            "mem_mb": self.tab_mem(i) / 1_048_576,
                            "active": i == self.active,
                            "claude_session": session,
                        })
                    })
                    .collect();
                let n = self.frame_us.len().max(1) as u64;
                let avg = self.frame_us.iter().sum::<u64>() / n;
                let max = self.frame_us.iter().copied().max().unwrap_or(0);
                let (hist, off) = self
                    .focused_pane()
                    .and_then(|p| p.live.as_ref())
                    .map(|l| {
                        let t = l.term.lock();
                        (t.grid().history_size(), t.grid().display_offset())
                    })
                    .unwrap_or((0, 0));
                let out = json!({"ok": true, "tabs": tabs, "pulseterm_mb": self.sys.self_mem / 1_048_576,
                       "frame_avg_ms": avg as f64 / 1000.0, "frame_max_ms": max as f64 / 1000.0,
                       "frames": self.frame_us.len(), "history_lines": hist, "scrolled_up": off,
                       "perf_ms": {"lock_wait": self.perf.0 as f64 / 1000.0, "copy": self.perf.1 as f64 / 1000.0,
                                   "draw": self.perf.2 as f64 / 1000.0, "rows_shaped": self.perf.3}});
                self.perf = (0, 0, 0, 0);
                self.frame_us.clear();
                out
            }
            "focus" => match self.tab_arg(j) {
                Some(i) => {
                    self.activate(i);
                    self.on_tray_focus_only();
                    json!({"ok": true})
                }
                None => json!({"ok": false, "error": "no such tab"}),
            },
            "new_tab" => {
                let prof = j.get("profile").and_then(|v| v.as_str()).map(String::from).unwrap_or(self.cfg.default_profile.clone());
                let cwd = j.get("cwd").and_then(|v| v.as_str()).map(String::from);
                if let Some(sid) = j.get("resume").and_then(|v| v.as_str()) {
                    self.resume_chat(sid, cwd);
                } else {
                    self.new_tab(&prof, cwd);
                }
                json!({"ok": true, "index": self.tabs.len()})
            }
            "send" | "paste" => {
                let Some(ti) = self.tab_arg(j) else { return json!({"ok": false, "error": "no such tab"}) };
                let pid = self.tabs[ti].focus;
                let text = j.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let Some(live) = self.panes.get(&pid).and_then(|p| p.live.as_ref()) else {
                    return json!({"ok": false, "error": "that tab is asleep"});
                };
                if cmd == "send" {
                    live.write(text.into_bytes());
                    return json!({"ok": true});
                }
                let image = j.get("image").and_then(|v| v.as_bool()).unwrap_or(false);
                let submit = j.get("submit").and_then(|v| v.as_bool()).unwrap_or(false);
                let tx = live.tx.clone();
                if image {
                    self.note_pasted_image(pid);
                }
                if j.get("focus").and_then(|v| v.as_bool()).unwrap_or(true) {
                    self.activate(ti);
                }
                // Text goes in as a bracketed paste, so it never touches the clipboard and
                // the clipboard keeps the picture. Then Alt+V tells Claude to paste the picture.
                std::thread::spawn(move || {
                    use alacritty_terminal::event_loop::Msg;
                    use std::borrow::Cow;
                    if !text.is_empty() {
                        let clean = text.replace("\r\n", "\r").replace('\n', "\r").replace('\x1b', "");
                        let _ = tx.send(Msg::Input(Cow::Owned(format!("\x1b[200~{clean}\x1b[201~").into_bytes())));
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    if image {
                        let _ = tx.send(Msg::Input(Cow::Borrowed(b"\x1bv")));
                    }
                    if submit {
                        std::thread::sleep(Duration::from_millis(if image { 2000 } else { 300 }));
                        let _ = tx.send(Msg::Input(Cow::Borrowed(b"\r")));
                    }
                });
                json!({"ok": true})
            }
            "import" => json!({"ok": true, "added": self.import_chats()}),
            // For testing the typing box: press a key, type text, read the box.
            "key" => {
                let vk = j.get("vk").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                let b = |k: &str| j.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
                let used = self.on_key(vk, b("ctrl"), b("shift"), b("alt"));
                json!({"ok": true, "used": used})
            }
            "type" => {
                let text = j.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                for u in text.encode_utf16() {
                    self.on_char(u, false);
                }
                json!({"ok": true})
            }
            "box" => self.box_state(),
            // Open Claude in a folder, as the folder menu does (for tests and scripts).
            "open_dir" => {
                let dir = j.get("dir").and_then(|v| v.as_str()).unwrap_or("").to_string();
                self.open_claude_in(&dir);
                json!({"ok": true, "tabs": self.tabs.len()})
            }
            // Thumbnails on screen, the open picture, and the toolbar (for tests).
            "thumbs" => json!({
                "ok": true,
                "thumbs": self.ui.thumbs.iter().map(|(r, p)| json!({"rect": [r.x, r.y, r.w, r.h], "title": p.title})).collect::<Vec<_>>(),
                "modal": self.ui.modal.as_ref().map(|p| p.title.clone()),
                "gutters": self.ui.gutters.iter().map(|(_, r)| [r.x, r.y, r.w, r.h]).collect::<Vec<_>>(),
                "tools": self.ui.tools.iter().map(|t| json!({"mic": [t.mic.x, t.mic.y, t.mic.w, t.mic.h], "rich": [t.rich.x, t.rich.y, t.rich.w, t.rich.h],
                    "folder": [t.folder.x, t.folder.y, t.folder.w, t.folder.h], "folder_path": self.pane_folder(t.id)})).collect::<Vec<_>>(),
            }),
            // Move the mouse to a point (tests), and say which picture preview it shows.
            "hover" => {
                let f = |k: &str| j.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                self.on_mouse_move(f("x"), f("y"));
                self.invalidate();
                json!({"ok": true, "preview": self.ui.preview.as_ref().map(|p| p.title.clone())})
            }
            "boxhit" => {
                let f = |k: &str| j.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                self.box_probe(f("x"), f("y"))
            }
            "voice" => {
                self.start_voice();
                json!({"ok": true})
            }
            "scroll" => {
                let s = match j.get("to").and_then(|v| v.as_str()) {
                    Some("top") => Scroll::Top,
                    Some("bottom") => Scroll::Bottom,
                    _ => Scroll::Delta(j.get("lines").and_then(|v| v.as_i64()).unwrap_or(0) as i32),
                };
                self.scroll(s);
                self.frame_us.clear();
                json!({"ok": true})
            }
            "zoom" => {
                self.toggle_zoom();
                json!({"ok": true})
            }
            "selection" => {
                let t = self.focused_pane().and_then(|p| p.live.as_ref()).and_then(|l| l.term.lock().selection_to_string());
                json!({"ok": true, "text": t})
            }
            "refresh" => {
                let Some(ti) = self.tab_arg(j) else { return json!({"ok": false, "error": "no such tab"}) };
                self.refresh_tab(ti);
                json!({"ok": true})
            }
            "split" | "sleep" | "resume" | "close_tab" => {
                let Some(ti) = self.tab_arg(j) else { return json!({"ok": false, "error": "no such tab"}) };
                match cmd {
                    "split" => {
                        self.activate(ti);
                        self.split(j.get("vertical").and_then(|v| v.as_bool()).unwrap_or(true));
                    }
                    "sleep" => self.sleep_tab(ti),
                    "resume" => {
                        for id in self.tabs[ti].root.leaves() {
                            self.resume(id);
                        }
                    }
                    _ => self.close_tab(ti, false),
                }
                json!({"ok": true})
            }
            "dump" => {
                let Some(ti) = self.tab_arg(j) else { return json!({"ok": false, "error": "no such tab"}) };
                let pid = self.tabs[ti].focus;
                let Some(p) = self.panes.get(&pid) else { return json!({"ok": false}) };
                let Some(live) = &p.live else { return json!({"ok": true, "sleeping": true, "lines": []}) };
                let t = live.term.lock();
                let g = t.grid();
                let off = g.display_offset() as i32;
                let mut lines = Vec::new();
                for r in 0..t.screen_lines() as i32 {
                    let row = &g[Line(r - off)];
                    let s: String = (0..t.columns()).map(|c| row[Column(c)].c).collect();
                    lines.push(s.trim_end().to_string());
                }
                let c = t.grid().cursor.point;
                let cell = &g[c.line][c.column];
                json!({"ok": true, "cols": t.columns(), "lines": lines, "mode": format!("{:?}", t.mode()),
                       "cursor": [c.line.0, c.column.0], "cursor_cell": format!("{:?} {:?}", cell.c, cell.flags),
                       "cursor_style": format!("{:?}", t.cursor_style())})
            }
            _ => json!({"ok": false, "error": format!("unknown cmd '{cmd}'")}),
        }
    }

    fn on_tray_focus_only(&self) {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::*;
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    // ---------------- keyboard ----------------

    pub fn write_focused(&mut self, bytes: Vec<u8>) {
        let Some(id) = self.focused_id() else { return };
        if let Some(p) = self.panes.get_mut(&id) {
            if let Some(l) = &p.live {
                {
                    let mut t = l.term.lock();
                    if t.grid().display_offset() != 0 {
                        t.scroll_display(Scroll::Bottom);
                    }
                    t.selection = None;
                }
                l.write(bytes);
            }
        }
    }

    fn app_cursor(&self) -> bool {
        self.focused_pane()
            .and_then(|p| p.live.as_ref())
            .is_some_and(|l| l.term.lock().mode().contains(TermMode::APP_CURSOR))
    }

    fn has_selection(&self) -> bool {
        self.focused_pane()
            .and_then(|p| p.live.as_ref())
            .is_some_and(|l| l.term.lock().selection.as_ref().is_some_and(|s| !s.is_empty()))
    }

    /// Called before TranslateMessage. Returns true when we used the key.
    pub fn on_keydown(&mut self, vk: u16) -> bool {
        let (ctrl, shift, alt) = (key(VK_CONTROL), key(VK_SHIFT), key(VK_MENU));
        self.on_key(vk, ctrl, shift, alt)
    }

    /// A key press with its Ctrl/Shift/Alt state. Returns true when we used the key.
    pub fn on_key(&mut self, vk: u16, ctrl: bool, shift: bool, alt: bool) -> bool {
        if vk == VK_CONTROL || vk == VK_SHIFT || vk == VK_MENU {
            if vk == VK_CONTROL {
                self.invalidate();
            }
            return false;
        }
        // A picture opened big: any key closes it (and does nothing else).
        // (A key we use is not turned into a typed letter, so nothing else reaches Claude.)
        if self.close_modal() {
            return true;
        }

        // Typing in a tab name.
        if let Some((idx, text)) = &mut self.ui.rename {
            let idx = *idx;
            match vk {
                VK_RETURN => {
                    let t = text.trim().to_string();
                    if let Some(tab) = self.tabs.get_mut(idx) {
                        tab.custom_title = if t.is_empty() { None } else { Some(t) };
                    }
                    self.ui.rename = None;
                }
                VK_ESCAPE => self.ui.rename = None,
                VK_BACK => {
                    text.pop();
                }
                _ => return false,
            }
            self.invalidate();
            return true;
        }

        // Typing in the find box.
        if self.ui.search.is_some() {
            match vk {
                VK_ESCAPE => {
                    self.ui.search = None;
                    self.invalidate();
                    return true;
                }
                VK_RETURN => {
                    self.search_step(!shift);
                    return true;
                }
                VK_BACK => {
                    if let Some(s) = &mut self.ui.search {
                        s.query.pop();
                    }
                    self.search_from_bottom();
                    return true;
                }
                _ if ctrl && shift && vk == b'F' as u16 => {
                    self.ui.search = None;
                    self.invalidate();
                    return true;
                }
                _ if !ctrl && !alt => return false,
                _ => {}
            }
        }

        // The typing box under a Claude tab.
        if self.box_key(vk, ctrl, shift, alt) {
            return true;
        }

        let altgr = ctrl && alt;
        if ctrl && shift && !alt {
            match vk {
                0x49 => self.box_toggle(), // I
                0x4F => self.pick_folder_for_claude(), // O
                0x54 => {
                    let p = self.cfg.default_profile.clone();
                    self.new_tab(&p, None)
                } // T
                0x57 => {
                    if let Some(id) = self.focused_id() {
                        self.close_pane(id, true)
                    }
                } // W
                0x44 => self.split(true),  // D
                0x45 => self.split(false), // E
                0x46 => self.open_search(), // F
                0x43 => self.copy(),       // C
                0x56 => self.paste(),      // V
                0x52 => self.start_rename(self.active), // R
                0x5A => self.toggle_zoom(),             // Z
                VK_TAB => self.activate((self.active + self.tabs.len() - 1) % self.tabs.len().max(1)),
                VK_PRIOR => self.move_tab(-1),
                VK_NEXT => self.move_tab(1),
                VK_HOME => self.scroll(Scroll::Top),
                VK_END => self.scroll(Scroll::Bottom),
                VK_SPACE => self.profile_menu_at_button(),
                _ => return false,
            }
            return true;
        }
        if ctrl && !shift && !alt {
            match vk {
                VK_TAB => {
                    let n = self.tabs.len().max(1);
                    self.activate((self.active + 1) % n);
                    return true;
                }
                0x31..=0x39 => {
                    let n = (vk - 0x30) as usize;
                    let idx = if n == 9 { self.tabs.len().saturating_sub(1) } else { n - 1 };
                    if idx < self.tabs.len() {
                        self.activate(idx);
                    }
                    return true;
                }
                VK_OEM_PLUS | VK_ADD => {
                    self.zoom(1.0);
                    return true;
                }
                VK_OEM_MINUS | VK_SUBTRACT => {
                    self.zoom(-1.0);
                    return true;
                }
                0x30 => {
                    self.zoom(0.0);
                    return true;
                }
                VK_OEM_COMMA => {
                    win::open_in_editor(&self.cfg.editor, &config::config_path().to_string_lossy(), None, None);
                    return true;
                }
                0x43 if self.has_selection() => {
                    self.copy();
                    if let Some(l) = self.focused_pane().and_then(|p| p.live.as_ref()) {
                        l.term.lock().selection = None;
                    }
                    self.invalidate();
                    return true;
                }
                0x56 => {
                    self.paste();
                    return true;
                }
                _ => {}
            }
        }
        if altgr && !shift {
            let d = match vk {
                VK_LEFT => Some((-1, 0)),
                VK_RIGHT => Some((1, 0)),
                VK_UP => Some((0, -1)),
                VK_DOWN => Some((0, 1)),
                _ => None,
            };
            if let Some((dx, dy)) = d {
                if let Some(cur) = self.focused_id() {
                    if let Some(n) = neighbor(&self.ui.panes, cur, dx, dy) {
                        self.focus_pane(n);
                    }
                }
                return true;
            }
        }
        if shift && !ctrl && !alt {
            match vk {
                VK_PRIOR => {
                    self.scroll(Scroll::PageUp);
                    return true;
                }
                VK_NEXT => {
                    self.scroll(Scroll::PageDown);
                    return true;
                }
                VK_INSERT => {
                    self.paste();
                    return true;
                }
                _ => {}
            }
        }

        // A sleeping pane: Enter wakes it.
        if let Some(p) = self.focused_pane() {
            if p.dormant.is_some() {
                if vk == VK_RETURN {
                    let id = p.id;
                    self.resume(id);
                }
                return true;
            }
        }

        if vk == VK_F10 && !ctrl {
            self.write_focused(b"\x1b[21~".to_vec());
            return true;
        }
        if let Some(bytes) = special(vk, shift, ctrl && !altgr, alt && !altgr, self.app_cursor()) {
            if vk == VK_BACK && !ctrl && !alt {
                self.guess_backspace();
            } else {
                self.guess_clear();
            }
            self.write_focused(bytes);
            return true;
        }
        false
    }

    pub fn on_char(&mut self, ch: u16, sys: bool) {
        if (0xD800..0xDC00).contains(&ch) {
            self.high_surrogate = Some(ch);
            return;
        }
        let s = if (0xDC00..0xE000).contains(&ch) {
            match self.high_surrogate.take() {
                Some(h) => String::from_utf16_lossy(&[h, ch]),
                None => return,
            }
        } else {
            String::from_utf16_lossy(&[ch])
        };
        if let Some((_, text)) = &mut self.ui.rename {
            if !s.chars().any(|c| c.is_control()) {
                text.push_str(&s);
                self.invalidate();
            }
            return;
        }
        if let Some(search) = &mut self.ui.search {
            if !s.chars().any(|c| c.is_control()) {
                search.query.push_str(&s);
                self.search_from_bottom();
            }
            return;
        }
        if self.focused_pane().is_some_and(|p| p.dormant.is_some()) {
            return;
        }
        if self.box_char(&s, sys) {
            return;
        }
        if sys {
            self.guess_clear();
        } else {
            self.guess_char(&s);
        }
        let mut bytes = Vec::new();
        if sys {
            bytes.push(0x1b);
        }
        bytes.extend_from_slice(s.as_bytes());
        self.write_focused(bytes);
    }

    // ---------------- clipboard ----------------

    pub fn copy(&mut self) {
        let text = self
            .focused_pane()
            .and_then(|p| p.live.as_ref())
            .and_then(|l| l.term.lock().selection_to_string());
        if let Some(t) = text {
            win::set_clipboard_text(self.hwnd, &t);
        }
    }

    pub fn paste(&mut self) {
        self.guess_clear();
        if let Some(id) = self.box_takes_keys() {
            self.box_paste(id);
            return;
        }
        let Some(id) = self.focused_id() else { return };
        match win::clipboard_text(self.hwnd) {
            Some(t) if !t.is_empty() => self.paste_text(id, t, true),
            _ => {
                if win::clipboard_has_image() {
                    // Claude Code reads pictures from the clipboard on Alt+V.
                    self.note_pasted_image(id);
                    self.write_focused(b"\x1bv".to_vec());
                }
            }
        }
    }

    pub fn paste_text(&mut self, id: u64, text: String, ask: bool) {
        let Some(p) = self.panes.get(&id) else { return };
        let Some(l) = &p.live else { return };
        let t = text.replace("\r\n", "\r").replace('\n', "\r");
        let bracketed = l.term.lock().mode().contains(TermMode::BRACKETED_PASTE) || p.stats.claude.is_some();
        let out = if bracketed {
            format!("\x1b[200~{}\x1b[201~", t.replace('\x1b', ""))
        } else {
            if ask && self.cfg.confirm_multiline_paste && t.trim_end_matches('\r').contains('\r') {
                let n = t.trim_end_matches('\r').matches('\r').count() + 1;
                self.pending.push(Deferred::Confirm {
                    text: format!("Paste {n} lines? In a shell, each line runs as a command."),
                    then: Confirmed::Paste(id, text),
                });
                return;
            }
            t
        };
        self.focus_pane(id);
        self.write_focused(out.into_bytes());
    }

    // ---------------- search ----------------

    fn open_search(&mut self) {
        if let Some(id) = self.focused_id() {
            if self.panes.get(&id).is_some_and(|p| p.live.is_some()) {
                self.ui.search = Some(Search { pane: id, query: String::new(), found: None });
                self.invalidate();
            }
        }
    }

    fn search_from_bottom(&mut self) {
        if let Some(l) = self.ui.search.as_ref().and_then(|s| self.panes.get(&s.pane)).and_then(|p| p.live.as_ref()) {
            let mut t = l.term.lock();
            t.selection = None;
            t.scroll_display(Scroll::Bottom);
        }
        self.search_step(true);
    }

    /// `up` = look at older lines.
    fn search_step(&mut self, up: bool) {
        let Some(s) = &mut self.ui.search else { return };
        let Some(l) = self.panes.get(&s.pane).and_then(|p| p.live.as_ref()) else { return };
        if s.query.is_empty() {
            s.found = None;
            l.term.lock().selection = None;
            self.invalidate();
            return;
        }
        let Ok(mut re) = RegexSearch::new(&regex::escape(&s.query)) else { return };
        let mut t = l.term.lock();
        let origin = match t.selection.as_ref().and_then(|x| x.to_range(&*t)) {
            Some(r) => {
                if up {
                    r.start
                } else {
                    r.end
                }
            }
            None => Point::new(Line(t.screen_lines() as i32 - 1), Column(t.columns() - 1)),
        };
        let (dir, side) = if up { (Direction::Left, Side::Left) } else { (Direction::Right, Side::Right) };
        let origin = if up {
            origin.sub(&*t, alacritty_terminal::index::Boundary::Grid, 1)
        } else {
            origin.add(&*t, alacritty_terminal::index::Boundary::Grid, 1)
        };
        let m = t.search_next(&mut re, origin, dir, side, None);
        s.found = Some(m.is_some());
        if let Some(m) = m {
            let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
            sel.update(*m.end(), Side::Right);
            t.selection = Some(sel);
            // Scroll so the match is on screen.
            let off = t.grid().display_offset() as i32;
            let row = m.start().line.0 + off;
            let lines = t.screen_lines() as i32;
            if row < 0 || row >= lines {
                let target = -m.start().line.0 + lines / 2;
                let delta = target - off;
                t.scroll_display(Scroll::Delta(delta));
            }
        }
        drop(t);
        self.invalidate();
    }

    // ---------------- misc commands ----------------

    fn scroll(&mut self, s: Scroll) {
        if let Some(l) = self.focused_pane().and_then(|p| p.live.as_ref()) {
            l.term.lock().scroll_display(s);
        }
        self.invalidate();
    }

    fn move_tab(&mut self, d: i32) {
        let n = self.tabs.len() as i32;
        let to = self.active as i32 + d;
        if to < 0 || to >= n {
            return;
        }
        self.tabs.swap(self.active, to as usize);
        self.active = to as usize;
        self.layout_bar();
        self.invalidate();
    }

    pub fn start_rename(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.ui.rename = Some((idx, self.tab_title(idx)));
            self.invalidate();
        }
    }

    pub fn focus_pane(&mut self, id: u64) {
        let Some(ti) = self.tab_of(id) else { return };
        let old = self.tabs[ti].focus;
        if old != id {
            self.focus_report(old, false);
            self.tabs[ti].focus = id;
            self.focus_report(id, true);
        }
        self.invalidate();
    }

    pub fn focus_report(&self, id: u64, on: bool) {
        if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
            if l.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
                l.write(if on { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() });
            }
        }
    }

    pub fn on_focus(&mut self, on: bool) {
        // The test copy never has the keyboard; draw it as if it had, so screenshots show the caret.
        self.focused = on || crate::dev();
        self.cursor_on = true;
        if let Some(id) = self.focused_id() {
            self.focus_report(id, on);
        }
        if on {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.alert = Alert::None;
            }
        }
        self.invalidate();
    }

    fn profile_menu_at_button(&mut self) {
        let r = self.ui.menu;
        let mut items: Vec<(u32, String)> = self.cfg.profiles.iter().enumerate().map(|(i, p)| (i as u32 + 1, p.name.clone())).collect();
        items.push((0, String::new()));
        items.extend(self.folder_items_for_new_tab());
        items.push((0, String::new()));
        items.push((100, "Bring in chats open in other windows (asleep)".into()));
        let (x, y) = self.to_screen(r.x, r.bottom());
        self.pending.push(Deferred::Menu { items, x, y, ctx: MenuCtx::Profiles });
    }

    pub fn to_screen(&self, x: f32, y: f32) -> (i32, i32) {
        let mut p = POINT { x: x as i32, y: y as i32 };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut p);
        }
        (p.x, p.y)
    }

    pub fn menu_command(&mut self, ctx: MenuCtx, cmd: u32) {
        if cmd == 0 {
            return;
        }
        match ctx {
            MenuCtx::Folder(id) => self.folder_command(id, cmd),
            MenuCtx::Profiles => {
                if cmd == 101 {
                    self.pick_folder_for_claude();
                    return;
                }
                if let Some(d) = self.recent_from_menu(cmd) {
                    self.open_claude_in(&d);
                    return;
                }
                if cmd == 100 {
                    let n = self.import_chats();
                    if n == 0 {
                        self.show_toast("No other Claude chats are open right now.".into());
                    } else {
                        self.show_toast(format!("Added {n} sleeping chat tab(s). Close them in the other window, then press Enter here."));
                    }
                    return;
                }
                if let Some(p) = self.cfg.profiles.get(cmd as usize - 1) {
                    let name = p.name.clone();
                    self.new_tab(&name, None);
                }
            }
            MenuCtx::Tab(i) => match cmd {
                1 => self.start_rename(i),
                2 => {
                    self.activate(i);
                    let (p, cwd) = self
                        .focused_pane()
                        .map(|p| (p.profile.clone(), p.cwd.clone()))
                        .unwrap_or((self.cfg.default_profile.clone(), None));
                    self.new_tab(&p, cwd);
                }
                3 => {
                    self.activate(i);
                    self.split(true)
                }
                4 => {
                    self.activate(i);
                    self.split(false)
                }
                5 => self.sleep_tab(i),
                10 => {
                    if self.tab_status(i) == "busy" {
                        self.pending.push(Deferred::Confirm {
                            text: "Claude is working in this tab. Refresh stops that work. Refresh anyway?".into(),
                            then: Confirmed::Refresh(i),
                        });
                    } else {
                        self.refresh_tab(i)
                    }
                }
                6 => self.close_tab(i, true),
                7 => {
                    let keep = self.tabs[i].id;
                    let others: Vec<usize> = (0..self.tabs.len()).filter(|j| self.tabs[*j].id != keep).collect();
                    for j in others.into_iter().rev() {
                        self.close_tab(j, false);
                    }
                }
                8 => {
                    self.cfg = config::load_config();
                    self.zoom(0.0);
                }
                9 => win::open_in_editor(&self.cfg.editor, &config::config_path().to_string_lossy(), None, None),
                _ => {}
            },
            MenuCtx::Pane(id) => match cmd {
                1 => self.copy(),
                2 => {
                    self.focus_pane(id);
                    self.paste()
                }
                3 => {
                    self.focus_pane(id);
                    self.split(true)
                }
                4 => {
                    self.focus_pane(id);
                    self.split(false)
                }
                5 => {
                    self.focus_pane(id);
                    self.open_search()
                }
                6 => {
                    if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
                        let mut t = l.term.lock();
                        let top = t.grid().topmost_line();
                        let bot = t.grid().bottommost_line();
                        let mut s = Selection::new(SelectionType::Lines, Point::new(top, Column(0)), Side::Left);
                        s.update(Point::new(bot, Column(0)), Side::Right);
                        t.selection = Some(s);
                    }
                    self.invalidate();
                }
                7 => self.close_pane(id, true),
                _ => {}
            },
        }
    }

    pub fn tab_menu(&mut self, i: usize, x: i32, y: i32) {
        let mem = self.tab_mem(i);
        let mut items = vec![
            (1, "Rename tab\tCtrl+Shift+R".to_string()),
            (2, "Duplicate tab".into()),
            (3, "Split right\tCtrl+Shift+D".into()),
            (4, "Split down\tCtrl+Shift+E".into()),
            (0, String::new()),
        ];
        if self.tabs[i].root.leaves().iter().any(|id| self.panes.get(id).is_some_and(|p| p.stats.claude.is_some())) {
            let gain = procmon::fmt_bytes(self.refresh_gain(i));
            items.push((10, format!("Refresh chat: restart Claude, keep the chat (frees about {gain})")));
        }
        if self.tab_status(i) != "sleeping" {
            items.push((5, format!("Sleep tab (frees {})", procmon::fmt_bytes(mem))));
        }
        items.extend([
            (6, "Close tab\tCtrl+Shift+W".to_string()),
            (7, "Close other tabs".into()),
            (0, String::new()),
            (9, "Open settings\tCtrl+,".into()),
            (8, "Reload settings".into()),
        ]);
        self.pending.push(Deferred::Menu { items, x, y, ctx: MenuCtx::Tab(i) });
    }

    pub fn pane_menu(&mut self, id: u64, x: i32, y: i32) {
        let items = vec![
            (1, "Copy\tCtrl+C".to_string()),
            (2, "Paste\tCtrl+V".into()),
            (0, String::new()),
            (3, "Split right\tCtrl+Shift+D".into()),
            (4, "Split down\tCtrl+Shift+E".into()),
            (5, "Find\tCtrl+Shift+F".into()),
            (6, "Select all".into()),
            (0, String::new()),
            (7, "Close pane\tCtrl+Shift+W".into()),
        ];
        self.pending.push(Deferred::Menu { items, x, y, ctx: MenuCtx::Pane(id) });
    }

    pub fn after_confirm(&mut self, then: Confirmed, yes: bool) {
        if !yes {
            return;
        }
        match then {
            Confirmed::ClosePane(id) => self.close_pane(id, false),
            Confirmed::CloseTab(i) => self.close_tab(i, false),
            Confirmed::CloseWindow => {
                self.quit_ok = true;
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
                }
            }
            Confirmed::Paste(id, t) => self.paste_text(id, t, false),
            Confirmed::Refresh(i) => self.refresh_tab(i),
            Confirmed::ResumeAnyway(id) => {
                if let Some(d) = self.panes.get_mut(&id).and_then(|p| p.dormant.as_mut()) {
                    d.elsewhere = None;
                }
                self.resume(id)
            }
        }
    }

    /// Window close: ask if Claude is working, then save tabs.
    pub fn on_close(&mut self) -> bool {
        if self.quit_ok {
            return true;
        }
        let busy = (0..self.tabs.len()).filter(|i| self.tab_status(*i) == "busy").count();
        if busy > 0 {
            self.pending.push(Deferred::Confirm {
                text: format!("Claude is still working in {busy} tab(s). Close PulseTerm anyway?"),
                then: Confirmed::CloseWindow,
            });
            return false;
        }
        self.quit_ok = true;
        true
    }

    pub fn shutdown(&mut self) {
        self.save_session();
        let ids: Vec<u64> = self.panes.keys().copied().collect();
        for id in ids {
            self.kill_pane(id);
        }
        win::tray_remove(self.hwnd);
    }

    // ---------------- mouse ----------------

    pub fn pane_at(&self, x: f32, y: f32) -> Option<(u64, Rect)> {
        self.ui.panes.iter().find(|(_, r)| r.contains(x, y)).copied()
    }

    pub fn grid_point(&self, id: u64, r: Rect, x: f32, y: f32) -> Option<(Point, Side)> {
        let p = self.panes.get(&id)?;
        let l = p.live.as_ref()?;
        let (ox, oy) = self.text_origin(r);
        let cf = (x - ox) / self.gfx.cell_w;
        let col = (cf.floor().max(0.0) as usize).min(p.cols.saturating_sub(1));
        let side = if cf - cf.floor() < 0.5 { Side::Left } else { Side::Right };
        let row = ((y - oy) / self.gfx.cell_h).floor() as i32;
        let row = row.clamp(0, p.lines as i32 - 1);
        let off = l.term.lock().grid().display_offset() as i32;
        Some((Point::new(Line(row - off), Column(col)), side))
    }

    fn mouse_mode(&self, id: u64) -> bool {
        self.panes
            .get(&id)
            .and_then(|p| p.live.as_ref())
            .is_some_and(|l| l.term.lock().mode().intersects(TermMode::MOUSE_MODE))
    }

    fn mouse_report(&self, id: u64, r: Rect, x: f32, y: f32, button: u8, press: bool) {
        let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) else { return };
        let (ox, oy) = self.text_origin(r);
        let col = ((x - ox) / self.gfx.cell_w).floor().max(0.0) as usize + 1;
        let row = ((y - oy) / self.gfx.cell_h).floor().max(0.0) as usize + 1;
        let sgr = l.term.lock().mode().contains(TermMode::SGR_MOUSE);
        let bytes = if sgr {
            format!("\x1b[<{};{};{}{}", button, col, row, if press { 'M' } else { 'm' }).into_bytes()
        } else {
            let b = if press { button } else { 3 };
            vec![0x1b, b'[', b'M', 32 + b, (32 + col.min(223)) as u8, (32 + row.min(223)) as u8]
        };
        l.write(bytes);
    }

    pub fn on_mouse_down(&mut self, x: f32, y: f32, button: u8) {
        self.ui.mouse = (x, y);
        // A picture opened big: a click anywhere closes it.
        if self.close_modal() {
            return;
        }
        self.guess_clear();
        if let Some((idx, _)) = &self.ui.rename {
            let idx = *idx;
            if !self.ui.tabs.get(idx).is_some_and(|t| t.rect.contains(x, y)) {
                let text = self.ui.rename.take().unwrap().1;
                if let Some(t) = self.tabs.get_mut(idx) {
                    t.custom_title = Some(text).filter(|s| !s.trim().is_empty());
                }
            }
        }
        if y < self.bar_h() {
            self.bar_click(x, y, button);
            return;
        }
        for (id, r) in self.ui.jump.clone() {
            if r.contains(x, y) && button == 0 {
                if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
                    l.term.lock().scroll_display(Scroll::Bottom);
                }
                self.invalidate();
                return;
            }
        }
        if button == 0 {
            for d in &self.ui.dividers {
                let grab = d.rect.inset(-self.s(3.0));
                if grab.contains(x, y) {
                    self.ui.drag = Drag::Divider(d.clone());
                    unsafe { SetCapture(self.hwnd) };
                    return;
                }
            }
        }
        // Click count for word / line selection (also used by the typing box).
        let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
        let count = if button == 0 {
            let (last, lx, ly, n) = self.ui.clicks;
            let dbl = unsafe { GetDoubleClickTime() };
            let c = if now.wrapping_sub(last) <= dbl && (lx - x).abs() < 5.0 && (ly - y).abs() < 5.0 { (n % 3) + 1 } else { 1 };
            self.ui.clicks = (now, x, y, c);
            c
        } else {
            1
        };
        if self.box_mouse_down(x, y, button, key(VK_SHIFT), count) {
            return;
        }
        if button == 0 && self.toolbar_mouse_down(x, y) {
            return;
        }
        if button == 0 {
            if let Some(pv) = self.thumb_at(x, y) {
                self.open_modal(pv);
                return;
            }
        }
        let Some((id, r)) = self.pane_at(x, y) else { return };
        self.focus_pane(id);
        if self.panes.get(&id).is_some_and(|p| p.dormant.is_some()) {
            if button == 0 {
                self.resume(id);
            }
            return;
        }
        if button != 0 {
            return;
        }
        // Claude (fullscreen) uses the mouse itself. Shift always selects here.
        let app_mouse = self.mouse_mode(id) && !key(VK_SHIFT);
        if app_mouse && !self.cfg.mouse_select {
            self.mouse_report(id, r, x, y, 0, true);
            self.ui.drag = Drag::App(id);
            unsafe { SetCapture(self.hwnd) };
            return;
        }
        // Scrollbar strip on the right edge.
        if x > r.right() - self.s(12.0) {
            let has_history = self.panes[&id].live.as_ref().is_some_and(|l| l.term.lock().grid().history_size() > 0);
            if has_history {
                self.ui.drag = Drag::Scroll(id);
                self.scrollbar_drag(id, r, y);
                unsafe { SetCapture(self.hwnd) };
                return;
            }
        }
        if key(VK_CONTROL) {
            if let Some((lid, link)) = self.ui.link.clone() {
                if lid == id {
                    self.open_link(&link.target);
                    return;
                }
            }
        }
        if app_mouse && count == 1 {
            // Wait and see: moving the mouse turns this into a text selection; letting go
            // without moving sends Claude a normal click. Double/triple-click select here.
            if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
                l.term.lock().selection = None;
            }
            self.ui.drag = Drag::Pending { id, x, y };
            unsafe { SetCapture(self.hwnd) };
            self.invalidate();
            return;
        }
        let Some((pt, side)) = self.grid_point(id, r, x, y) else { return };
        let ty = match count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
            let mut t = l.term.lock();
            if key(VK_SHIFT) && t.selection.is_some() {
                if let Some(s) = &mut t.selection {
                    s.update(pt, side);
                }
            } else {
                t.selection = if count == 1 { Some(Selection::new(ty, pt, side)) } else { Some(Selection::new(ty, pt, side)) };
            }
        }
        self.ui.drag = Drag::Select(id);
        unsafe { SetCapture(self.hwnd) };
        self.invalidate();
    }

    fn bar_click(&mut self, x: f32, y: f32, button: u8) {
        for (i, t) in self.ui.tabs.clone().iter().enumerate() {
            if !t.rect.contains(x, y) {
                continue;
            }
            match button {
                0 => {
                    if t.close.contains(x, y) {
                        self.close_tab(i, true);
                        return;
                    }
                    if t.badge.contains(x, y) && self.refresh_hint(i) {
                        let gain = procmon::fmt_bytes(self.refresh_gain(i));
                        self.pending.push(Deferred::Confirm {
                            text: format!("Refresh this chat?

Claude restarts with the same chat (10-20 seconds) and gives back about {gain} of RAM."),
                            then: Confirmed::Refresh(i),
                        });
                        return;
                    }
                    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
                    let (last, lx, _, _) = self.ui.clicks;
                    if now.wrapping_sub(last) <= unsafe { GetDoubleClickTime() } && (lx - x).abs() < 5.0 {
                        self.start_rename(i);
                        self.ui.clicks = (0, 0.0, 0.0, 0);
                        return;
                    }
                    self.ui.clicks = (now, x, y, 1);
                    self.activate(i);
                    self.ui.drag = Drag::Tab { idx: i, start_x: x, moved: false };
                    unsafe { SetCapture(self.hwnd) };
                }
                1 => self.close_tab(i, true),
                _ => {}
            }
            return;
        }
        if button == 0 && self.ui.plus.contains(x, y) {
            let p = self.cfg.default_profile.clone();
            self.new_tab(&p, None);
        } else if self.ui.menu.contains(x, y) || (button == 2 && self.ui.plus.contains(x, y)) {
            self.profile_menu_at_button();
        }
    }

    fn scrollbar_drag(&mut self, id: u64, r: Rect, y: f32) {
        if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
            let mut t = l.term.lock();
            let hist = t.grid().history_size() as f32;
            let lines = t.screen_lines() as f32;
            let frac = ((y - r.y) / r.h).clamp(0.0, 1.0);
            // frac 0 = top of history, 1 = bottom.
            let target_off = ((1.0 - frac) * (hist + lines) - lines / 2.0).clamp(0.0, hist).round() as i32;
            let cur = t.grid().display_offset() as i32;
            t.scroll_display(Scroll::Delta(target_off - cur));
        }
        self.invalidate();
    }

    pub fn on_mouse_move(&mut self, x: f32, y: f32) {
        self.ui.mouse = (x, y);
        match self.ui.drag.clone() {
            Drag::Divider(d) => {
                let ratio = if d.vertical { (x - d.parent.x) / d.parent.w } else { (y - d.parent.y) / d.parent.h };
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.root.set_ratio(&d.path, ratio);
                }
                self.relayout();
                self.invalidate();
                return;
            }
            Drag::Tab { idx, start_x, moved } => {
                if !moved && (x - start_x).abs() < self.s(6.0) {
                    return;
                }
                self.ui.drag = Drag::Tab { idx, start_x, moved: true };
                let mut to = idx;
                if idx + 1 < self.ui.tabs.len() && x > self.ui.tabs[idx + 1].rect.cx() {
                    to = idx + 1;
                } else if idx > 0 && x < self.ui.tabs[idx - 1].rect.cx() {
                    to = idx - 1;
                }
                if to != idx {
                    self.tabs.swap(idx, to);
                    self.active = to;
                    self.ui.drag = Drag::Tab { idx: to, start_x: x, moved: true };
                    self.layout_bar();
                }
                self.invalidate();
                return;
            }
            Drag::Scroll(id) => {
                if let Some((_, r)) = self.ui.panes.iter().find(|(p, _)| *p == id).copied() {
                    self.scrollbar_drag(id, r, y);
                }
                return;
            }
            Drag::Compose(id) => {
                self.box_drag(id, x, y);
                return;
            }
            Drag::Pending { id, x: x0, y: y0 } => {
                let far = self.s(4.0).max(self.gfx.cell_w * 0.5);
                if (x - x0).abs() < far && (y - y0).abs() < far {
                    return;
                }
                // It is a drag: select text, starting where the button went down.
                if let Some((_, r)) = self.ui.panes.iter().find(|(p, _)| *p == id).copied() {
                    if let Some((pt, side)) = self.grid_point(id, r, x0, y0) {
                        if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
                            l.term.lock().selection = Some(Selection::new(SelectionType::Simple, pt, side));
                        }
                    }
                }
                self.ui.drag = Drag::Select(id);
                self.on_mouse_move(x, y);
                return;
            }
            Drag::App(id) => {
                let wants_moves = self.panes.get(&id).and_then(|p| p.live.as_ref()).is_some_and(|l| {
                    l.term.lock().mode().intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
                });
                if wants_moves {
                    if let Some((_, r)) = self.ui.panes.iter().find(|(p, _)| *p == id).copied() {
                        self.mouse_report(id, r, x, y, 32, true);
                    }
                }
                return;
            }
            Drag::Select(id) => {
                if let Some((_, r)) = self.ui.panes.iter().find(|(p, _)| *p == id).copied() {
                    let (_, oy) = self.text_origin(r);
                    if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
                        // Drag above or below the pane to scroll.
                        if y < oy {
                            l.term.lock().scroll_display(Scroll::Delta(1));
                        } else if y > r.bottom() {
                            l.term.lock().scroll_display(Scroll::Delta(-1));
                        }
                    }
                    if let Some((pt, side)) = self.grid_point(id, r, x, y) {
                        if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
                            if let Some(s) = &mut l.term.lock().selection {
                                s.update(pt, side);
                            }
                        }
                    }
                    self.invalidate();
                }
                return;
            }
            Drag::None => {}
        }
        // Hover: picture previews, links (with Ctrl held), tabs.
        self.ui.preview = self.preview_at(x, y);
        let old = self.ui.link.take();
        if key(VK_CONTROL) {
            if let Some((id, r)) = self.pane_at(x, y) {
                if let Some((pt, _)) = self.grid_point(id, r, x, y) {
                    let cwd = self.panes[&id].cwd.clone();
                    if let Some(l) = self.panes[&id].live.as_ref() {
                        let t = l.term.lock();
                        if let Some(link) = links::find_at(&*t, pt, cwd.as_deref()) {
                            self.ui.link = Some((id, link));
                        }
                    }
                }
            }
        }
        if old != self.ui.link || y < self.bar_h() + self.s(4.0) || true {
            self.invalidate();
        }
    }

    pub fn on_mouse_up(&mut self, x: f32, y: f32, button: u8) {
        let drag = std::mem::take(&mut self.ui.drag);
        unsafe {
            let _ = ReleaseCapture();
        }
        if button == 2 {
            let (sx, sy) = self.to_screen(x, y);
            if y < self.bar_h() {
                if let Some(i) = self.ui.tabs.iter().position(|t| t.rect.contains(x, y)) {
                    self.tab_menu(i, sx, sy);
                } else if self.ui.plus.contains(x, y) {
                    self.profile_menu_at_button();
                }
            } else if let Some((id, _)) = self.pane_at(x, y) {
                self.pane_menu(id, sx, sy);
            }
            return;
        }
        if button == 1 && y < self.bar_h() {
            if let Some(i) = self.ui.tabs.iter().position(|t| t.rect.contains(x, y)) {
                self.close_tab(i, true);
            }
            return;
        }
        match drag {
            Drag::Pending { id, x: x0, y: y0 } => {
                // A plain click: Claude gets the press and the release.
                if let Some((_, r)) = self.ui.panes.iter().find(|(p, _)| *p == id).copied() {
                    self.mouse_report(id, r, x0, y0, 0, true);
                    self.mouse_report(id, r, x0, y0, 0, false);
                }
                self.invalidate();
                return;
            }
            Drag::App(id) => {
                if let Some((_, r)) = self.ui.panes.iter().find(|(p, _)| *p == id).copied() {
                    self.mouse_report(id, r, x, y, 0, false);
                }
                return;
            }
            _ => {}
        }
        if let Drag::Select(id) = drag {
            if let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) {
                let mut t = l.term.lock();
                if t.selection.as_ref().is_some_and(|s| s.is_empty()) {
                    t.selection = None;
                }
            }
            if self.cfg.copy_on_select {
                self.copy();
            }
        }
        self.invalidate();
    }

    pub fn on_wheel(&mut self, x: f32, y: f32, delta: i16) {
        if self.box_wheel(x, y, delta) {
            return;
        }
        let Some((id, r)) = self.pane_at(x, y) else { return };
        let Some(p) = self.panes.get_mut(&id) else { return };
        let Some(l) = &p.live else { return };
        p.scroll_acc += delta as f32 / 120.0 * 3.0;
        let n = p.scroll_acc.trunc() as i32;
        if n == 0 {
            return;
        }
        p.scroll_acc -= n as f32;
        let mode = *l.term.lock().mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            let b = if n > 0 { 64 } else { 65 };
            for _ in 0..n.abs() {
                self.mouse_report(id, r, x, y, b, true);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let seq: &[u8] = if n > 0 { b"\x1bOA" } else { b"\x1bOB" };
            let mut out = Vec::new();
            for _ in 0..n.abs() {
                out.extend_from_slice(seq);
            }
            l.write(out);
        } else {
            l.term.lock().scroll_display(Scroll::Delta(n));
        }
        self.invalidate();
    }

    fn open_link(&self, t: &Target) {
        match t {
            Target::Url(u) => win::shell_open(u),
            Target::File { path, line, col } => win::open_in_editor(&self.cfg.editor, path, *line, *col),
        }
    }

    pub fn on_drop(&mut self, files: Vec<String>) {
        let Some(id) = self.focused_id() else { return };
        let text = files.iter().map(|f| if f.contains(' ') { format!("\"{f}\"") } else { f.clone() }).collect::<Vec<_>>().join(" ");
        if self.box_takes_keys() == Some(id) {
            self.box_char(&format!("{text} "), false);
            return;
        }
        self.paste_text(id, text, false);
    }

    pub fn cursor_for(&self, x: f32, y: f32) -> u16 {
        // 0 arrow, 1 ibeam, 2 hand, 3 size-we, 4 size-ns
        if y < self.bar_h() {
            return 0;
        }
        for d in &self.ui.dividers {
            if d.rect.inset(-self.s(3.0)).contains(x, y) {
                return if d.vertical { 3 } else { 4 };
            }
        }
        if self.ui.link.is_some() && key(VK_CONTROL) {
            return 2;
        }
        if self.ui.modal.is_some() {
            return 0;
        }
        if let Some(b) = self.box_at(x, y) {
            return if b.mic.contains(x, y) { 2 } else { 1 };
        }
        if let Some(t) = self.toolbar_at(x, y) {
            return if t.mic.contains(x, y) || t.rich.contains(x, y) || t.folder.contains(x, y) { 2 } else { 0 };
        }
        if self.thumb_at(x, y).is_some() {
            return 2;
        }
        if let Some((id, r)) = self.pane_at(x, y) {
            if self.panes.get(&id).is_some_and(|p| p.dormant.is_some()) {
                return 2;
            }
            if x > r.right() - self.s(12.0) {
                return 0;
            }
            return 1;
        }
        0
    }
}
