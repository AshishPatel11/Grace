//! Speech to text on this PC with Whisper (whisper.cpp), the kind of AI model modern
//! voice tools use. Good with noisy mics and accents, and the voice never leaves the PC.
//!
//! Why not Windows' own engines: on some PCs the speech engine apps may use hears
//! nothing (Win+H uses a newer one apps cannot call), and the old offline engine turns
//! room noise into words.
//!
//! What it needs is downloaded once, on first start, into
//! `%LOCALAPPDATA%\Grace\whisper` (about 160 MB):
//! - the official whisper.cpp Windows build (`whisper-cli.exe` and its DLLs),
//! - the English model (`ggml-base.en.bin`, or `small.en` with `voice_model`),
//! - a small voice detector (Silero VAD) that cuts out the parts with no speech, so
//!   room noise is not turned into words.
//!
//! Every file is checked against a fixed SHA-256 fingerprint (the same ones GitHub and
//! Hugging Face list), so a broken or changed download is never used.
//!
//! Each transcription runs `whisper-cli.exe` on the recorded .wav: about 1.2 s for 7 s
//! of speech on a 6-core Intel i5-12400F with the base model.

use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::pane::AppEvent;
use crate::voice::VoiceEvent;

/// No console window for the helper programs.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// One file to download.
pub struct Asset {
    /// Name on disk (in the whisper folder).
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// whisper.cpp build b5454 (2026-10-06), CPU version for 64-bit Windows.
const ENGINE: Asset = Asset {
    file: "whisper-bin-x64-b5454.zip",
    url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip",
    sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
    size: 8_928_640,
};
/// The folder the engine zip is unpacked into.
const ENGINE_DIR: &str = "b5454";

const VAD: Asset = Asset {
    file: "ggml-silero-v5.1.2.bin",
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
    sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
    size: 885_098,
};

const BASE_EN: Asset = Asset {
    file: "ggml-base.en.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
    sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
    size: 147_964_211,
};

const SMALL_EN: Asset = Asset {
    file: "ggml-small.en.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
    sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
    size: 487_614_201,
};

/// The model for `voice_model`: "small.en" (better, 3x bigger and slower) or the
/// default "base.en".
pub fn model(name: &str) -> &'static Asset {
    match name.trim().to_ascii_lowercase().as_str() {
        "small.en" | "small" => &SMALL_EN,
        _ => &BASE_EN,
    }
}

/// `%LOCALAPPDATA%\Grace\whisper`. Until the first normal start after the rename moves
/// it (config::move_old_folders), the download may still sit under the old name
/// (PulseTerm): use it there rather than download it again.
pub fn dir() -> PathBuf {
    let base = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into()));
    let new = base.join("Grace").join("whisper");
    let old = base.join("PulseTerm").join("whisper");
    if !new.exists() && old.exists() {
        old
    } else {
        new
    }
}

fn cli() -> PathBuf {
    dir().join(ENGINE_DIR).join("Release").join("whisper-cli.exe")
}

fn has(a: &Asset) -> bool {
    std::fs::metadata(dir().join(a.file)).is_ok_and(|m| m.len() == a.size)
}

/// Is everything for `model_name` on disk?
pub fn ready(model_name: &str) -> bool {
    cli().exists() && has(model(model_name)) && has(&VAD)
}

// ---------------- the download ----------------

static DOWNLOADING: AtomicBool = AtomicBool::new(false);
/// What the download is busy with, for the progress message: (file in progress, bytes
/// of the files already done, bytes of all files).
static PROGRESS: Mutex<Option<(PathBuf, u64, u64)>> = Mutex::new(None);

/// "Downloading the speech model: 45% of 157 MB", while it downloads.
pub fn progress_text() -> Option<String> {
    if !DOWNLOADING.load(Ordering::SeqCst) {
        return None;
    }
    let (part, done, total) = PROGRESS.lock().unwrap().clone()?;
    let now = done + std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    Some(format!("Downloading the speech model for the mic: {}% of {} MB. This happens once.", now * 100 / total.max(1), total / 1_000_000))
}

fn system_tool(name: &str) -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Download one file to `<file>.part` with Windows' own curl, check its fingerprint,
/// then give it its real name.
fn fetch(a: &Asset) -> Result<(), String> {
    let dest = dir().join(a.file);
    let part = dir().join(format!("{}.part", a.file));
    let status = Command::new(system_tool("curl.exe"))
        .args(["-sSL", "--fail", "--retry", "3", "--connect-timeout", "20", "-o"])
        .arg(&part)
        .arg(a.url)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download of {} failed (curl exit {:?})", a.file, status.code()));
    }
    let got = sha256_of(&part).map_err(|e| e.to_string())?;
    if got != a.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{} did not match its fingerprint, so it was thrown away", a.file));
    }
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())
}

/// Unpack the engine zip with Windows' own tar.
fn unpack_engine() -> Result<(), String> {
    let tmp = dir().join(format!("{ENGINE_DIR}.tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let status = Command::new(system_tool("tar.exe"))
        .arg("-xf")
        .arg(dir().join(ENGINE.file))
        .arg("-C")
        .arg(&tmp)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("could not run tar: {e}"))?;
    if !status.success() || !tmp.join("Release").join("whisper-cli.exe").exists() {
        return Err("could not unpack the speech engine".into());
    }
    let _ = std::fs::remove_dir_all(dir().join(ENGINE_DIR));
    std::fs::rename(&tmp, dir().join(ENGINE_DIR)).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(dir().join(ENGINE.file));
    Ok(())
}

/// Start downloading whatever is missing, in the background (once at a time). The
/// window hears about the end through a `VoiceEvent::Setup`. Returns true when a
/// download started.
pub fn ensure(model_name: &str, tx: Sender<AppEvent>) -> bool {
    if ready(model_name) || DOWNLOADING.swap(true, Ordering::SeqCst) {
        return false;
    }
    let m = model(model_name);
    let spawned = std::thread::Builder::new().name("whisper-setup".into()).spawn(move || {
        let result = (|| {
            std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
            let mut todo: Vec<&Asset> = Vec::new();
            if !cli().exists() {
                todo.push(&ENGINE);
            }
            for a in [&VAD, m] {
                if !has(a) {
                    todo.push(a);
                }
            }
            let total: u64 = todo.iter().map(|a| a.size).sum();
            let mut done = 0;
            for a in todo {
                *PROGRESS.lock().unwrap() = Some((dir().join(format!("{}.part", a.file)), done, total));
                crate::log(&format!("whisper: downloading {}", a.file));
                fetch(a)?;
                if a.file == ENGINE.file {
                    unpack_engine()?;
                }
                done += a.size;
            }
            Ok::<(), String>(())
        })();
        if let Err(e) = &result {
            crate::log(&format!("whisper: setup failed: {e}"));
        } else {
            crate::log("whisper: ready");
        }
        *PROGRESS.lock().unwrap() = None;
        DOWNLOADING.store(false, Ordering::SeqCst);
        let _ = tx.send(AppEvent::Voice(VoiceEvent::Setup { error: result.err() }));
        crate::pane::wake();
    });
    if spawned.is_err() {
        DOWNLOADING.store(false, Ordering::SeqCst);
        return false;
    }
    true
}

// ---------------- speech to text ----------------

/// Turn a recorded .wav (16 kHz mono) into text. Blocks (run it on the voice thread).
pub fn transcribe(wav: &Path, model_name: &str) -> Result<String, String> {
    // Whisper works best with one thread per real core (half the logical ones).
    let threads = std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(4).clamp(2, 8);
    let out = Command::new(cli())
        .arg("-m")
        .arg(dir().join(model(model_name).file))
        .arg("-f")
        .arg(wav)
        .args(["-l", "en", "-t", &threads.to_string(), "-nt", "-np", "-sns", "--vad", "-vm"])
        .arg(dir().join(VAD.file))
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("could not run the speech engine: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return Err(format!("the speech engine failed: {last}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whisper's output as one line, or None when it holds no words: only a tag like
/// "[BLANK_AUDIO]" or "(music)", or only punctuation.
pub fn clean(raw: &str) -> Option<String> {
    let joined = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = joined.trim();
    let is_tag = |s: &str| (s.starts_with('[') && s.ends_with(']')) || (s.starts_with('(') && s.ends_with(')'));
    if t.is_empty() || is_tag(t) || !t.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    Some(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_whisper_output() {
        assert_eq!(clean("\n Please list all the files.\n Then show me.\n").as_deref(), Some("Please list all the files. Then show me."));
        assert_eq!(clean(" [BLANK_AUDIO]\n"), None);
        assert_eq!(clean("(music)"), None);
        assert_eq!(clean(" ... "), None);
        assert_eq!(clean(""), None);
    }

    #[test]
    fn picks_the_model() {
        assert_eq!(model("").file, "ggml-base.en.bin");
        assert_eq!(model("small.en").file, "ggml-small.en.bin");
        assert_eq!(model("Small").file, "ggml-small.en.bin");
        assert_eq!(model("tiny").file, "ggml-base.en.bin");
    }
}
