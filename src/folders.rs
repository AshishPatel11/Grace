//! "Open Claude in another folder".
//!
//! Claude cannot move to another folder once it runs (its chat belongs to the folder it
//! started in). So "change folder" opens a NEW tab with a fresh Claude in that folder,
//! and the chat you were in keeps running in its own tab.
//!
//! Where you can do it:
//! - The folder button on the toolbar under a Claude tab (shows that tab's folder).
//! - The ⌄ menu next to +.
//! - Ctrl+Shift+O (Windows' folder picker).
//!
//! Recent folders are kept in `%APPDATA%\Grace\recent_folders.json` (newest first).

use std::path::{Path, PathBuf};

use crate::app::{App, Deferred, MenuCtx};
use crate::config::Config;

const MAX_RECENT: usize = 8;

fn recent_path() -> PathBuf {
    crate::config::data_dir().join("recent_folders.json")
}

pub fn load_recent() -> Vec<String> {
    let list: Vec<String> = std::fs::read_to_string(recent_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    list.into_iter().filter(|d| Path::new(d).is_dir()).collect()
}

/// Put `dir` first in the recent list (no repeats, at most 8).
pub fn remember(dir: &str) {
    let mut list = load_recent();
    list.retain(|d| !d.eq_ignore_ascii_case(dir));
    list.insert(0, dir.to_string());
    list.truncate(MAX_RECENT);
    if let Ok(s) = serde_json::to_string_pretty(&list) {
        let _ = std::fs::write(recent_path(), s);
    }
}

/// "J:\Projects\Classpulse" -> "Classpulse"; "J:\" stays "J:\".
pub fn short_name(dir: &str) -> String {
    let t = dir.trim_end_matches(['\\', '/']);
    match Path::new(t).file_name() {
        Some(n) => n.to_string_lossy().to_string(),
        None => dir.to_string(),
    }
}

impl App {
    /// The folder a pane works in: Claude's own folder if it runs, else the folder it started in.
    pub fn pane_folder(&self, id: u64) -> Option<String> {
        let p = self.panes.get(&id)?;
        p.stats.claude.as_ref().map(|c| c.cwd.clone()).filter(|c| !c.is_empty()).or_else(|| p.cwd.clone())
    }

    /// The name of the profile that runs Claude (usually "Claude").
    fn claude_profile(&self) -> String {
        self.cfg
            .profiles
            .iter()
            .find(|p| Config::is_claude_command(&p.command))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| self.cfg.default_profile.clone())
    }

    /// Open a new tab with Claude running in `dir`.
    pub fn open_claude_in(&mut self, dir: &str) {
        if !Path::new(dir).is_dir() {
            self.show_toast(format!("That folder does not exist: {dir}"));
            return;
        }
        remember(dir);
        let prof = self.claude_profile();
        self.new_tab(&prof, Some(dir.to_string()));
        self.show_toast(format!("Claude opened in {}", short_name(dir)));
    }

    /// Ask Windows' folder picker, then open Claude there. The picker runs its own
    /// message loop, so it is started later from the main loop (like menus).
    pub fn pick_folder_for_claude(&mut self) {
        let start = self.focused_id().and_then(|id| self.pane_folder(id));
        self.pending.push(Deferred::PickFolder { start });
    }

    /// Menu items for the recent folders (ids 200..), skipping `skip`. Remembers the list
    /// so the chosen id can be turned back into a folder.
    fn recent_items(&mut self, skip: Option<&str>) -> Vec<(u32, String)> {
        let mut list = load_recent();
        // Folders of the open tabs count as recent too.
        for t in &self.tabs {
            for id in t.root.leaves() {
                if let Some(d) = self.pane_folder(id) {
                    if Path::new(&d).is_dir() && !list.iter().any(|x| x.eq_ignore_ascii_case(&d)) {
                        list.push(d);
                    }
                }
            }
        }
        list.retain(|d| skip.map_or(true, |s| !d.eq_ignore_ascii_case(s)));
        list.truncate(MAX_RECENT);
        self.ui.folder_menu = list.clone();
        list.into_iter().enumerate().map(|(i, d)| (200 + i as u32, format!("Open Claude in   {d}"))).collect()
    }

    /// The menu of the folder button under a Claude tab.
    pub fn folder_menu(&mut self, id: u64, x: i32, y: i32) {
        let here = self.pane_folder(id);
        let mut items = vec![(1, "Open Claude in another folder…\tCtrl+Shift+O".to_string())];
        let recent = self.recent_items(here.as_deref());
        if !recent.is_empty() {
            items.push((0, String::new()));
            items.extend(recent);
        }
        if here.is_some() {
            items.push((0, String::new()));
            items.push((2, "Show this folder in Explorer".into()));
            items.push((3, "Copy this folder's path".into()));
        }
        self.pending.push(Deferred::Menu { items, x, y, ctx: MenuCtx::Folder(id) });
    }

    /// Items for the ⌄ menu next to +: the picker and the recent folders.
    pub fn folder_items_for_new_tab(&mut self) -> Vec<(u32, String)> {
        let mut items = vec![(101, "Claude in another folder…\tCtrl+Shift+O".to_string())];
        let recent = self.recent_items(None);
        items.extend(recent.into_iter().take(5));
        items
    }

    /// A recent-folder menu id (200..) back to its folder.
    pub fn recent_from_menu(&self, cmd: u32) -> Option<String> {
        cmd.checked_sub(200).and_then(|i| self.ui.folder_menu.get(i as usize).cloned())
    }

    pub fn folder_command(&mut self, id: u64, cmd: u32) {
        match cmd {
            1 => self.pick_folder_for_claude(),
            2 => {
                if let Some(d) = self.pane_folder(id) {
                    crate::win::shell_open(&d);
                }
            }
            3 => {
                if let Some(d) = self.pane_folder(id) {
                    crate::win::set_clipboard_text(self.hwnd, &d);
                    self.show_toast("Folder path copied.".into());
                }
            }
            c if c >= 200 => {
                if let Some(d) = self.recent_from_menu(c) {
                    self.open_claude_in(&d);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names() {
        assert_eq!(short_name("J:\\Projects\\Classpulse"), "Classpulse");
        assert_eq!(short_name("J:\\Projects\\Classpulse\\"), "Classpulse");
        assert_eq!(short_name("J:\\"), "J:\\");
    }
}
