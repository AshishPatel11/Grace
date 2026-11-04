//! Settings file and saved tabs.
//!
//! Settings live in `%APPDATA%\Grace\config.toml`. The first run writes a
//! commented default file, so the user can see every option.
//! Open tabs are saved to `%APPDATA%\Grace\session.json` when the window closes.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub font_family: String,
    /// Font size in points.
    pub font_size: f32,
    /// How many old lines each pane keeps. Older lines are dropped, so memory stays flat.
    pub scrollback_lines: usize,
    /// Name of the profile a new tab uses.
    pub default_profile: String,
    /// The command used to resume a sleeping Claude chat (`--resume <id>` is added).
    pub claude_command: String,
    /// Shell started after a Claude-only tab's chat ends.
    pub fallback_shell: String,
    /// A tab's RAM badge turns amber above this, red above twice this.
    pub ram_warn_mb: u64,
    /// "code" opens files in VS Code at the right line. Empty = the default app.
    pub editor: String,
    pub copy_on_select: bool,
    pub confirm_multiline_paste: bool,
    pub cursor_blink: bool,
    pub restore_tabs: bool,
    /// Windows popup when Claude finishes or needs you while you look elsewhere.
    pub notify_popup: bool,
    /// Draw symbols and emoji once and reuse the picture (about 30x faster). Turn off if they look wrong.
    pub fast_symbols: bool,
    /// Open the "Rich input" box under each Claude tab from the start: text is edited
    /// there like in Word and sent on Enter. Off = type straight into Claude's own line;
    /// the toolbar button or Ctrl+Shift+I opens the box for one tab.
    pub input_box: bool,
    /// A thin bar under each Claude tab with the mic and "Rich input" buttons.
    pub toolbar: bool,
    /// Letters typed into Claude's own input line show at once, before Claude draws them
    /// (Claude can stall for a few hundred ms in a long chat).
    pub instant_typing: bool,
    /// Small pictures at the right edge, next to each line with `[Image #N]` or an image
    /// file path. Click one to see it big.
    pub thumbnails: bool,
    /// Drag with the mouse selects text even when the program (Claude) uses the mouse.
    /// A click without dragging still goes to the program.
    pub mouse_select: bool,
    /// The mic button. "auto" = Whisper on this PC (downloaded on first start, about
    /// 160 MB). Or Windows' own engines: "windows" (modern, else offline), "modern",
    /// "offline", or "win+h" (open Windows' voice typing panel, the old way).
    pub voice_engine: String,
    /// The Whisper model: "base.en" (148 MB, fast) or "small.en" (488 MB, better with
    /// accents and noise, about 3x slower).
    pub voice_model: String,
    /// Language for the modern speech engine, like "en-IN" or "en-US".
    /// "" = Windows' speech language.
    pub voice_language: String,
    /// Push to talk: hold Right Alt to talk, let go to type it. (A quick tap, or Right
    /// Alt used with another key, like AltGr+E, types nothing.)
    pub voice_hold: bool,
    pub profiles: Vec<Profile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Profile {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub cwd: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into());
        let work = if Path::new("J:\\Projects\\Classpulse").exists() {
            "J:\\Projects\\Classpulse".to_string()
        } else {
            home
        };
        let mut profiles = vec![
            Profile {
                name: "Claude".into(),
                command: "claude --dangerously-skip-permissions".into(),
                cwd: Some(work.clone()),
            },
            Profile { name: "PowerShell".into(), command: "powershell.exe -NoLogo".into(), cwd: Some(work) },
        ];
        if Path::new("C:\\Program Files\\Git\\bin\\bash.exe").exists() {
            profiles.push(Profile {
                name: "Git Bash".into(),
                command: "\"C:\\Program Files\\Git\\bin\\bash.exe\" --login -i".into(),
                cwd: None,
            });
        }
        profiles.push(Profile { name: "Command Prompt".into(), command: "cmd.exe".into(), cwd: None });
        Config {
            font_family: "Cascadia Mono".into(),
            font_size: 11.0,
            scrollback_lines: 10_000,
            default_profile: "Claude".into(),
            claude_command: "claude --dangerously-skip-permissions".into(),
            fallback_shell: "powershell.exe -NoLogo".into(),
            ram_warn_mb: 1024,
            editor: "code".into(),
            copy_on_select: false,
            confirm_multiline_paste: true,
            cursor_blink: true,
            restore_tabs: true,
            notify_popup: true,
            fast_symbols: true,
            input_box: false,
            toolbar: true,
            instant_typing: true,
            thumbnails: true,
            mouse_select: true,
            voice_engine: "auto".into(),
            voice_model: "base.en".into(),
            voice_language: String::new(),
            voice_hold: true,
            profiles,
        }
    }
}

impl Config {
    pub fn profile(&self, name: &str) -> Profile {
        self.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .or_else(|| self.profiles.first())
            .cloned()
            .unwrap_or(Profile { name: "PowerShell".into(), command: "powershell.exe -NoLogo".into(), cwd: None })
    }

    /// True when the profile starts Claude directly (no shell under it).
    pub fn is_claude_command(command: &str) -> bool {
        let first = command.trim().trim_start_matches('"').split([' ', '"']).next().unwrap_or("");
        let exe = first.rsplit(['\\', '/']).next().unwrap_or("").to_ascii_lowercase();
        exe == "claude" || exe == "claude.exe"
    }
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(base).join(if crate::dev() { "Grace-dev" } else { "Grace" });
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Grace was called PulseTerm before. On the first start after the rename, move the old
/// folders to the new names, so settings, saved tabs, recent folders and the Whisper
/// download carry over. The test copy (`--dev`) moves only its own folder.
pub fn move_old_folders() {
    let mut moves = vec![("APPDATA", "PulseTerm-dev", "Grace-dev")];
    if !crate::dev() {
        moves = vec![("APPDATA", "PulseTerm", "Grace"), ("LOCALAPPDATA", "PulseTerm", "Grace")];
    }
    for (var, old, new) in moves {
        let Ok(base) = std::env::var(var) else { continue };
        let (from, to) = (Path::new(&base).join(old), Path::new(&base).join(new));
        if !from.is_dir() || to.exists() {
            continue;
        }
        // A rename is instant. If something holds a file open, copy instead.
        let how = if std::fs::rename(&from, &to).is_ok() {
            "moved"
        } else if copy_dir(&from, &to).is_ok() {
            "copied"
        } else {
            "could not move"
        };
        crate::log(&format!("rename: {how} {} to {}", from.display(), to.display()));
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest)?;
        }
    }
    Ok(())
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn load_config() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            crate::log(&format!("config.toml has an error, using defaults: {e}"));
            Config::default()
        }),
        Err(_) => {
            let cfg = Config::default();
            let _ = std::fs::write(&path, default_file_text(&cfg));
            cfg
        }
    }
}

fn default_file_text(cfg: &Config) -> String {
    let body = toml::to_string_pretty(cfg).unwrap_or_default();
    format!(
        "# Grace settings. Save this file, then restart Grace (or right-click a tab > Reload settings).\n\
         #\n\
         # font_size        : points. Ctrl+= / Ctrl+- also change it for this run.\n\
         # scrollback_lines : old lines kept per pane. More lines = more RAM.\n\
         # ram_warn_mb      : tab RAM badge turns amber above this, red above twice this.\n\
         # editor           : \"code\" opens clicked files in VS Code at the line. \"\" = default app.\n\
         # voice_engine     : the mic. \"auto\" = Whisper on this PC (downloaded once, about 160 MB).\n\
         #                    Or Windows' engines: \"windows\", \"modern\", \"offline\", \"win+h\".\n\
         # voice_model      : Whisper model: \"base.en\" (fast) or \"small.en\" (better, 488 MB, slower).\n\
         # voice_language   : like \"en-IN\" for Windows' modern engine. \"\" = Windows' speech language.\n\
         # voice_hold       : hold Right Alt to talk, let go to type it (push to talk).\n\
         # profiles         : what a new tab runs. default_profile picks the one for Ctrl+Shift+T.\n\n{body}"
    )
}

// ---------- saved tabs ----------

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SavedSession {
    pub window: Option<SavedWindow>,
    pub tabs: Vec<SavedTab>,
    pub active: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct SavedWindow {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub maximized: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SavedTab {
    pub title: Option<String>,
    pub layout: SavedNode,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum SavedNode {
    Leaf(SavedPane),
    Split { vertical: bool, ratio: f32, a: Box<SavedNode>, b: Box<SavedNode> },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SavedPane {
    pub profile: String,
    pub cwd: Option<String>,
    pub claude_session: Option<String>,
    pub claude_name: Option<String>,
}

pub fn session_path() -> PathBuf {
    data_dir().join("session.json")
}

pub fn load_session() -> Option<SavedSession> {
    let text = std::fs::read_to_string(session_path()).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_session(s: &SavedSession) {
    if let Ok(text) = serde_json::to_string_pretty(s) {
        // Write to a temp file first, so a crash mid-write never leaves a broken file.
        let tmp = data_dir().join("session.json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, session_path());
        }
    }
}
