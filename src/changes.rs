//! "Changes": the files changed in a tab's git folder (the viewer is in diffview.rs).
//!
//! The toolbar button only shows in a git folder (found by looking for a `.git` folder, no
//! process). git runs only when you click it, never in the background (Warp ran a new
//! PowerShell every second for its git info).

use std::path::{Path, PathBuf};

use crate::app::App;

#[derive(Clone, Debug, PartialEq)]
pub struct Changed {
    /// Path from the repo's top folder, with "/".
    pub path: String,
    /// Two letters from `git status` ("M ", " M", "??", "A ", " D", "R "...).
    pub status: String,
    /// Lines added and removed (None for new files and binary files).
    pub lines: Option<(u32, u32)>,
}

impl Changed {
    pub fn new_file(&self) -> bool {
        self.status == "??" || self.status.starts_with('A')
    }
    fn deleted(&self) -> bool {
        self.status.contains('D')
    }
    /// One letter for the list: M, A (new), D or R.
    pub fn letter(&self) -> char {
        if self.new_file() {
            'A'
        } else if self.deleted() {
            'D'
        } else if self.status.contains('R') {
            'R'
        } else {
            'M'
        }
    }
}

/// The top folder of the git repo `dir` is in (has `.git`), without starting git.
pub fn repo_root(dir: &str) -> Option<PathBuf> {
    let mut p = Path::new(dir);
    loop {
        if p.join(".git").exists() {
            return Some(p.to_path_buf());
        }
        p = p.parent()?;
    }
}

/// `git status --porcelain=v1 -z` output: entries "XY path\0", renames "XY new\0old\0".
pub fn parse_status(out: &str) -> Vec<(String, String)> {
    let mut items = Vec::new();
    let mut parts = out.split('\0');
    while let Some(e) = parts.next() {
        if e.len() < 4 {
            continue;
        }
        let (xy, path) = (&e[..2], &e[3..]);
        if xy.contains('R') || xy.contains('C') {
            parts.next(); // the old name
        }
        items.push((xy.to_string(), path.to_string()));
    }
    items
}

/// `git diff HEAD --numstat -z` output: "added\tremoved\tpath\0", renames
/// "added\tremoved\t\0old\0new\0". Binary files show "-". Returns path -> (added, removed).
pub fn parse_numstat(out: &str) -> Vec<(String, Option<(u32, u32)>)> {
    let mut items = Vec::new();
    let mut parts = out.split('\0');
    while let Some(e) = parts.next() {
        let mut f = e.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (f.next(), f.next(), f.next()) else { continue };
        let path = if path.is_empty() {
            parts.next();
            parts.next().unwrap_or("").to_string()
        } else {
            path.to_string()
        };
        let lines = a.parse().ok().zip(r.parse().ok());
        items.push((path, lines));
    }
    items
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW: no console flashing up
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The changed files of the repo at `root` (runs git twice).
pub fn changed_files(root: &Path) -> Option<Vec<Changed>> {
    let status = git(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    // No commit yet: no HEAD to compare with; line counts are left out.
    let numstat = git(root, &["diff", "HEAD", "--numstat", "-z"]).unwrap_or_default();
    let counts = parse_numstat(&numstat);
    let mut files: Vec<Changed> = parse_status(&status)
        .into_iter()
        .map(|(status, path)| {
            let lines = counts.iter().find(|(p, _)| *p == path).and_then(|(_, l)| *l);
            Changed { path, status, lines }
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Some(files)
}

impl App {
    /// The repo of pane `id`'s folder, if it is in one (for the toolbar button).
    pub fn pane_repo(&self, id: u64) -> Option<PathBuf> {
        repo_root(&self.pane_folder(id)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_git_status() {
        let out = " M src/app.rs\0?? src/new.rs\0R  b.rs\0a.rs\0 D gone.txt\0";
        assert_eq!(
            parse_status(out),
            vec![
                (" M".to_string(), "src/app.rs".to_string()),
                ("??".to_string(), "src/new.rs".to_string()),
                ("R ".to_string(), "b.rs".to_string()),
                (" D".to_string(), "gone.txt".to_string()),
            ]
        );
    }

    #[test]
    fn reads_line_counts() {
        let out = "61\t3\tsrc/app.rs\0-\t-\tlogo.png\0" .to_string() + "2\t1\t\0old.rs\0new.rs\0";
        assert_eq!(
            parse_numstat(&out),
            vec![("src/app.rs".to_string(), Some((61, 3))), ("logo.png".to_string(), None), ("new.rs".to_string(), Some((2, 1)))]
        );
    }
}
