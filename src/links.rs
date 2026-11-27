//! Find a clickable link (URL or file path) under the mouse.

use std::path::Path;
use std::sync::OnceLock;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::Term;
use regex::Regex;

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Url(String),
    File { path: String, line: Option<u32>, col: Option<u32> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub start: Point,
    pub end: Point,
    pub target: Target,
}

fn res() -> &'static [Regex; 3] {
    static R: OnceLock<[Regex; 3]> = OnceLock::new();
    R.get_or_init(|| {
        [
            Regex::new(r#"https?://[^\s<>"'`]+"#).unwrap(),
            Regex::new(r#"(?:[A-Za-z]:[\\/]|\\\\)[^\s<>"'`|*?]+"#).unwrap(),
            Regex::new(r#"(?:\.{1,2}[\\/])?(?:[\w@.\-]+[\\/])+[\w@.\-]+\.[A-Za-z0-9]{1,8}(?::\d+){0,2}"#).unwrap(),
        ]
    })
}

fn trim_end(s: &str) -> &str {
    let mut s = s;
    loop {
        let Some(last) = s.chars().last() else { return s };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | ']' | '}' | '>' => true,
            ')' => s.matches('(').count() < s.matches(')').count(),
            _ => false,
        };
        if !drop {
            return s;
        }
        s = &s[..s.len() - last.len_utf8()];
    }
}

/// "C:\a\b.ts:42:7" -> ("C:\a\b.ts", 42, 7)
fn split_line_col(s: &str) -> (String, Option<u32>, Option<u32>) {
    let mut parts: Vec<&str> = s.rsplitn(3, ':').collect();
    parts.reverse();
    let num = |x: &str| x.parse::<u32>().ok();
    match parts.as_slice() {
        [p, l, c] if num(l).is_some() && num(c).is_some() && p.len() > 2 => (p.to_string(), num(l), num(c)),
        _ => match s.rsplit_once(':') {
            Some((p, l)) if num(l).is_some() && p.len() > 2 => (p.to_string(), num(l), None),
            _ => (s.to_string(), None, None),
        },
    }
}

fn file_url_to_path(u: &str) -> String {
    let p = u.trim_start_matches("file:///").trim_start_matches("file://");
    let mut out = String::new();
    let b = p.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&p[i + 1..i + 3], 16) {
                out.push(v as char);
                i += 3;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out.replace('/', "\\")
}

/// Image file paths in one screen row that exist on disk (full paths, or paths relative
/// to `cwd`). Used for thumbnails.
pub fn image_paths_in(text: &str, cwd: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut full_spans: Vec<(usize, usize)> = Vec::new();
    for (i, re) in res().iter().enumerate().skip(1) {
        for m in re.find_iter(text) {
            // A relative match inside a full path is the same path: skip it.
            if i == 2 && full_spans.iter().any(|(s, e)| m.start() < *e && m.end() > *s) {
                continue;
            }
            let s = trim_end(m.as_str());
            let (path, _, _) = split_line_col(s);
            if !crate::images::is_image_path(&path) {
                continue;
            }
            let full = if i == 2 {
                let Some(base) = cwd else { continue };
                Path::new(base).join(path.replace('/', "\\")).to_string_lossy().to_string()
            } else {
                full_spans.push((m.start(), m.end()));
                path
            };
            if !out.contains(&full) && Path::new(&full).is_file() {
                out.push(full);
            }
        }
    }
    out
}

/// What opening a file does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OpenWith {
    /// A folder: File Explorer.
    Explorer,
    /// Code and text: the editor (VS Code), at the line if there is one.
    Editor,
    /// Everything else (PDF, video, pictures, Office files): the app Windows uses for it.
    DefaultApp,
}

/// Files that are code or plain text. These open in the editor even when Windows has an
/// app for them (Windows thinks ".ts" is a video, for example).
fn is_code(path: &str) -> bool {
    const CODE: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "kts", "c", "h", "cc", "cpp", "hpp", "cs", "rb",
        "php", "swift", "dart", "lua", "sh", "bash", "ps1", "psm1", "bat", "cmd", "sql", "json", "jsonc", "jsonl", "toml", "yaml",
        "yml", "ini", "cfg", "conf", "env", "md", "mdx", "txt", "log", "csv", "tsv", "xml", "html", "htm", "css", "scss", "sass",
        "less", "vue", "svelte", "astro", "graphql", "prisma", "proto", "gradle", "lock", "gitignore", "dockerfile", "makefile",
    ];
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase();
    let ext = name.rsplit_once('.').map_or(name.as_str(), |(_, e)| e);
    CODE.contains(&ext)
}

/// How to open `path`. `has_app`: Windows has an app set for this kind of file.
pub fn open_with(path: &str, line: Option<u32>, is_dir: bool, has_app: bool) -> OpenWith {
    if is_dir {
        OpenWith::Explorer
    } else if line.is_some() || is_code(path) || !has_app {
        OpenWith::Editor
    } else {
        OpenWith::DefaultApp
    }
}

/// The file's extension with the dot (".pdf"), for asking Windows about its app.
pub fn ext_of(path: &str) -> Option<String> {
    let name = path.rsplit(['\\', '/']).next()?;
    name.rsplit_once('.').filter(|(b, e)| !b.is_empty() && !e.is_empty()).map(|(_, e)| format!(".{e}"))
}

/// The text a link covers on screen (to tell later whether it is still there).
pub fn text_of<T>(term: &Term<T>, l: &Link) -> String {
    let grid = term.grid();
    let cols = grid.columns();
    let mut s = String::new();
    for line in l.start.line.0..=l.end.line.0 {
        let c0 = if line == l.start.line.0 { l.start.column.0 } else { 0 };
        let c1 = if line == l.end.line.0 { l.end.column.0 } else { cols - 1 };
        for c in c0..=c1.min(cols - 1) {
            let cell = &grid[Line(line)][Column(c)];
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                s.push(cell.c);
            }
        }
    }
    s
}

pub fn find_at<T>(term: &Term<T>, at: Point, cwd: Option<&str>) -> Option<Link> {
    let grid = term.grid();
    let cols = grid.columns();
    if at.column.0 >= cols || at.line < grid.topmost_line() || at.line > grid.bottommost_line() {
        return None;
    }

    // 1) A real hyperlink sent by the program (OSC 8).
    if let Some(h) = grid[at.line][at.column].hyperlink() {
        let id = h.id().to_string();
        let same = |p: Point| grid[p.line][p.column].hyperlink().is_some_and(|x| x.id() == id);
        let (mut s, mut e) = (at, at);
        while s.column.0 > 0 && same(Point::new(s.line, Column(s.column.0 - 1))) {
            s.column.0 -= 1;
        }
        while e.column.0 + 1 < cols && same(Point::new(e.line, Column(e.column.0 + 1))) {
            e.column.0 += 1;
        }
        let uri = h.uri().to_string();
        let target = if uri.starts_with("file:") {
            let (path, line, col) = split_line_col(&file_url_to_path(&uri));
            Target::File { path, line, col }
        } else {
            Target::Url(uri)
        };
        return Some(Link { start: s, end: e, target });
    }

    // 2) Plain text. Join wrapped rows into one logical line first.
    let wraps = |l: Line| grid[l][Column(cols - 1)].flags.contains(Flags::WRAPLINE);
    let mut top = at.line;
    while top > grid.topmost_line() && wraps(Line(top.0 - 1)) {
        top = Line(top.0 - 1);
    }
    let mut bot = at.line;
    while bot < grid.bottommost_line() && wraps(bot) {
        bot = Line(bot.0 + 1);
    }
    let mut text = String::new();
    let mut map: Vec<(usize, Point)> = Vec::new();
    for l in top.0..=bot.0 {
        for c in 0..cols {
            let cell = &grid[Line(l)][Column(c)];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            map.push((text.len(), Point::new(Line(l), Column(c))));
            text.push(cell.c);
        }
    }
    let here = map.iter().find(|(_, p)| *p == at)?.0;
    let point_of = |byte: usize| map.iter().rev().find(|(b, _)| *b <= byte).map(|(_, p)| *p);

    for (i, re) in res().iter().enumerate() {
        for m in re.find_iter(&text) {
            let s = trim_end(m.as_str());
            if here < m.start() || here >= m.start() + s.len() {
                continue;
            }
            let start = point_of(m.start())?;
            let end = point_of(m.start() + s.len() - 1)?;
            let target = if i == 0 {
                Target::Url(s.to_string())
            } else {
                let (path, line, col) = split_line_col(s);
                let full = if i == 2 {
                    let base = cwd?;
                    Path::new(base).join(path.replace('/', "\\")).to_string_lossy().to_string()
                } else {
                    path
                };
                if !Path::new(&full).exists() {
                    continue;
                }
                Target::File { path: full, line, col }
            };
            return Some(Link { start, end, target });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_right_app() {
        // PDF, video, Word: the app Windows uses for them.
        assert_eq!(open_with(r"C:\a\report.pdf", None, false, true), OpenWith::DefaultApp);
        assert_eq!(open_with(r"C:\a\clip.mp4", None, false, true), OpenWith::DefaultApp);
        assert_eq!(open_with(r"C:\a\b.docx", None, false, true), OpenWith::DefaultApp);
        // Code: the editor, even when Windows has an app (".ts" is a video for Windows).
        assert_eq!(open_with(r"C:\a\app.ts", None, false, true), OpenWith::Editor);
        assert_eq!(open_with(r"C:\a\README.md", None, false, true), OpenWith::Editor);
        // A line number means "show me this line": the editor.
        assert_eq!(open_with(r"C:\a\x.weird", Some(4), false, true), OpenWith::Editor);
        // No app for it: the editor.
        assert_eq!(open_with(r"C:\a\notes.zzz", None, false, false), OpenWith::Editor);
        assert_eq!(open_with(r"C:\a", None, true, false), OpenWith::Explorer);
    }

    #[test]
    fn extensions() {
        assert_eq!(ext_of(r"C:\a.b\file.PDF").as_deref(), Some(".PDF"));
        assert_eq!(ext_of(r"C:\a.b\Makefile"), None);
        assert_eq!(ext_of(r"C:\a\.gitignore"), None);
    }
}
