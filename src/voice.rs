//! Talk instead of typing: the mic button listens and types what you say.
//!
//! Before, the mic pressed Win+H for you. That opened Windows' floating voice panel:
//! you had to click inside it, and the words did not always reach the terminal. Now
//! PulseTerm listens by itself, with Windows' own speech engines:
//!
//! 1. The modern engine (`Windows.Media.SpeechRecognition`). Good results, but Windows
//!    allows it only when "Online speech recognition" is on (Settings > Privacy &
//!    security > Speech). Your voice then goes to Microsoft to be turned into text.
//! 2. The old offline engine (SAPI 5, the one behind "Windows Speech Recognition").
//!    Always there, nothing leaves the PC, but it makes more mistakes.
//!
//! `voice_engine = "auto"` (the default) tries 1, and uses 2 when Windows says no.
//!
//! How it runs: the engine lives on its own thread. What it hears comes back to the
//! window as events: the words so far while you talk (shown in the toolbar), then the
//! final words when you pause, which are typed into Claude's input line (or into the
//! Rich input box when it is open). Click the mic again, press Esc, or stay quiet for
//! 10 seconds to stop.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::Column;
use windows::core::{IUnknown, Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Media::Speech::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};

use crate::app::App;
use crate::pane::AppEvent;

/// Stop listening after this long without speech.
const SILENCE: Duration = Duration::from_secs(10);
/// After you click stop (or let go of Right Alt) in the middle of a sentence, wait this
/// long for its last words.
const FINISH: Duration = Duration::from_millis(2000);
/// Right Alt let go sooner than this: a tap, not talk. Nothing is typed.
const TAP: Duration = Duration::from_millis(300);
/// Windows' "the speech privacy policy was not accepted" error: Online speech
/// recognition is off, so the modern engine may not listen.
const PRIVACY_OFF: u32 = 0x8004_5509;
/// Shown once when the offline engine is used because the modern one is not allowed.
const TURN_ON: &str = "For better results, turn on Online speech recognition (Settings > Privacy & security > Speech).";

/// Which engine `voice_engine` asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// The modern engine when Windows allows it, else the offline one.
    Auto,
    Modern,
    Offline,
    /// The old way: open Windows' voice typing panel (Win+H).
    WinH,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s.trim().to_ascii_lowercase().as_str() {
            "modern" | "online" => Mode::Modern,
            "offline" | "sapi" => Mode::Offline,
            "win+h" | "winh" | "windows" => Mode::WinH,
            _ => Mode::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Engine {
    Modern,
    Offline,
}

/// What the listening thread tells the window. `session` tells old listens apart.
pub enum VoiceEvent {
    /// Listening now. `note`: why the modern engine was not used (in auto mode).
    Started { session: u64, engine: Engine, note: Option<String> },
    /// The words so far, while you talk ("" = nothing pending).
    Partial { session: u64, text: String },
    /// Final words for one phrase (you paused).
    Heard { session: u64, text: String },
    /// Listening is over (stopped, silence, or an error).
    Ended { session: u64, error: Option<String> },
}

impl VoiceEvent {
    fn session(&self) -> u64 {
        match self {
            VoiceEvent::Started { session, .. }
            | VoiceEvent::Partial { session, .. }
            | VoiceEvent::Heard { session, .. }
            | VoiceEvent::Ended { session, .. } => *session,
        }
    }
}

/// The listen in progress (kept by the window).
pub struct Voice {
    pub session: u64,
    /// The pane that gets the words (the one focused when you clicked the mic).
    pub pane: u64,
    /// None while the engine starts.
    pub engine: Option<Engine>,
    /// The words so far, not final yet.
    pub partial: String,
    /// Stop was asked; waiting for the last words.
    pub stopping: bool,
    /// Push to talk: when Right Alt went down (it stops when Right Alt comes up).
    pub hold: Option<Instant>,
    /// Throw away what is heard (a tap, or Right Alt was part of a shortcut).
    pub discard: bool,
    stop: Sender<()>,
}

/// Is this key message Right Alt going down (Some(true)) or up (Some(false))?
/// Right Alt is Alt with the "extended key" bit (24). A held key repeats its "down"
/// with bit 30 set: only the first one counts. With a UK or German layout Right Alt is
/// AltGr, and Windows sends it as WM_KEYDOWN (not WM_SYSKEYDOWN), so both are checked.
pub fn right_alt(msg: u32, wp: usize, lp: isize) -> Option<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP};
    if wp != 0x12 || (lp >> 24) & 1 == 0 {
        return None;
    }
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN if (lp >> 30) & 1 == 0 => Some(true),
        WM_KEYUP | WM_SYSKEYUP => Some(false),
        _ => None,
    }
}

/// What to type for heard words: trimmed, with a space in front when they follow
/// other text (`before` = the character just before where they go).
pub fn phrase_to_type(text: &str, before: Option<char>) -> Option<String> {
    let t: String = text.trim().chars().filter(|c| !c.is_control()).collect();
    if t.is_empty() {
        return None;
    }
    Some(match before {
        Some(c) if !c.is_whitespace() && c != '\0' => format!(" {t}"),
        _ => t,
    })
}

/// The end of `text` that fits in `width` (measured by `w`), with "…" in front when
/// cut. The newest words are at the end, so those are the ones to keep.
pub fn tail_that_fits(text: &str, width: f32, w: impl Fn(&str) -> f32) -> String {
    if w(text) <= width {
        return text.to_string();
    }
    let mut words: Vec<&str> = text.split(' ').collect();
    while words.len() > 1 {
        words.remove(0);
        let s = format!("…{}", words.join(" "));
        if w(&s) <= width {
            return s;
        }
    }
    String::new()
}

// ---------------- the listening thread ----------------

/// Sends events to the window and wakes it.
#[derive(Clone)]
struct Out(Sender<AppEvent>);

impl Out {
    fn send(&self, ev: VoiceEvent) {
        let _ = self.0.send(AppEvent::Voice(ev));
        crate::pane::wake();
    }
}

/// Start listening on a new thread. Send `()` on the returned sender to stop.
/// `wav`: listen to this file instead of the mic (tests).
fn start(session: u64, mode: Mode, language: String, wav: Option<String>, tx: Sender<AppEvent>) -> Sender<()> {
    let (stop_tx, stop_rx) = channel::<()>();
    let wake = stop_tx.clone();
    let out = Out(tx);
    let spawned = std::thread::Builder::new().name("voice".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let result = listen(session, mode, &language, wav, &out, &stop_rx, wake);
        out.send(VoiceEvent::Ended { session, error: result.err() });
        unsafe { CoUninitialize() };
    });
    if spawned.is_err() {
        crate::log("voice: could not start the listening thread");
    }
    stop_tx
}

/// Is "Online speech recognition" on? (Settings > Privacy & security > Speech.) The
/// modern engine refuses to listen without it, and finding that out the slow way costs
/// about a second, so look at the setting first.
fn online_speech_allowed() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut v: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Software\\Microsoft\\Speech_OneCore\\Settings\\OnlineSpeechPrivacy"),
            windows::core::w!("HasAccepted"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    r.is_ok() && v == 1
}

/// Pick the engine and listen until stopped. Runs on the voice thread.
fn listen(session: u64, mode: Mode, language: &str, wav: Option<String>, out: &Out, stop: &Receiver<()>, wake: Sender<()>) -> Result<(), String> {
    let t0 = Instant::now();
    let mut note = None;
    // Only the offline engine can listen to a file.
    if wav.is_none() && matches!(mode, Mode::Auto | Mode::Modern) {
        if mode == Mode::Auto && !online_speech_allowed() {
            note = Some(TURN_ON.into());
        } else {
            match modern_start(session, language, out, wake) {
                Ok(m) => {
                    crate::log(&format!("voice: modern engine listening after {} ms", t0.elapsed().as_millis()));
                    out.send(VoiceEvent::Started { session, engine: Engine::Modern, note: None });
                    return modern_wait(m, stop);
                }
                Err(e) if mode == Mode::Auto => {
                    crate::log(&format!("voice: modern engine not used: {}", explain(&e)));
                    note = Some(if e.code().0 as u32 == PRIVACY_OFF { TURN_ON.into() } else { format!("The modern engine could not start: {}", explain(&e)) });
                }
                Err(e) => return Err(format!("Voice input could not start: {}", explain(&e))),
            }
        }
    }
    let had_note = note.is_some();
    unsafe { offline_run(session, wav.as_deref(), out, stop, note, t0) }.map_err(|e| {
        if had_note {
            format!("Voice input could not start. Offline engine: {}", explain(&e))
        } else {
            format!("Voice input could not start: {}", explain(&e))
        }
    })
}

/// A Windows error in plain words.
fn explain(e: &windows::core::Error) -> String {
    let code = e.code().0 as u32;
    if code == PRIVACY_OFF {
        return "Online speech recognition is off (Settings > Privacy & security > Speech).".into();
    }
    let m = e.message();
    let m = m.trim();
    format!("{} (0x{code:08X})", if m.is_empty() { "speech engine error" } else { m })
}

// ---------------- 1. the modern engine ----------------

struct ModernRun {
    rec: windows::Media::SpeechRecognition::SpeechRecognizer,
    session: windows::Media::SpeechRecognition::SpeechContinuousRecognitionSession,
    done: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

/// Start the modern engine. Fails (so we can fall back) when Windows does not allow it.
fn modern_start(session: u64, language: &str, out: &Out, wake: Sender<()>) -> windows::core::Result<ModernRun> {
    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Media::SpeechRecognition::*;

    let rec = if language.trim().is_empty() {
        SpeechRecognizer::new()?
    } else {
        let lang = windows::Globalization::Language::CreateLanguage(&HSTRING::from(language.trim()))?;
        SpeechRecognizer::Create(&lang)?
    };
    // No constraint added = free dictation.
    let compiled = rec.CompileConstraintsAsync()?.join()?;
    let status = compiled.Status()?;
    if status != SpeechRecognitionResultStatus::Success {
        return Err(windows::core::Error::new(windows::Win32::Foundation::E_FAIL, format!("the modern engine could not get ready (status {})", status.0)));
    }
    let s = rec.ContinuousRecognitionSession()?;
    s.SetAutoStopSilenceTimeout(TimeSpan { Duration: SILENCE.as_millis() as i64 * 10_000 })?;

    let o = out.clone();
    rec.HypothesisGenerated(&TypedEventHandler::<SpeechRecognizer, SpeechRecognitionHypothesisGeneratedEventArgs>::new(move |_, a| {
        if let Some(a) = a.as_ref() {
            o.send(VoiceEvent::Partial { session, text: a.Hypothesis()?.Text()?.to_string() });
        }
        Ok(())
    }))?;
    let o = out.clone();
    s.ResultGenerated(&TypedEventHandler::<SpeechContinuousRecognitionSession, SpeechContinuousRecognitionResultGeneratedEventArgs>::new(
        move |_, a| {
            if let Some(a) = a.as_ref() {
                let r = a.Result()?;
                let sure = r.Confidence()? != SpeechRecognitionConfidence::Rejected;
                let text = if sure { r.Text()?.to_string() } else { String::new() };
                if text.trim().is_empty() {
                    o.send(VoiceEvent::Partial { session, text: String::new() });
                } else {
                    o.send(VoiceEvent::Heard { session, text });
                }
            }
            Ok(())
        },
    ))?;
    let done = Arc::new(AtomicBool::new(false));
    let error = Arc::new(Mutex::new(None));
    let (d, e) = (done.clone(), error.clone());
    s.Completed(&TypedEventHandler::<SpeechContinuousRecognitionSession, SpeechContinuousRecognitionCompletedEventArgs>::new(move |_, a| {
        if let Some(a) = a.as_ref() {
            if let Some(msg) = modern_problem(a.Status()?) {
                *e.lock().unwrap() = Some(msg);
            }
        }
        d.store(true, Ordering::SeqCst);
        let _ = wake.send(());
        Ok(())
    }))?;
    // This is where Windows says no when Online speech recognition is off.
    s.StartAsync()?.join()?;
    Ok(ModernRun { rec, session: s, done, error })
}

/// Why the modern engine stopped, when it is a problem (None = a normal stop).
fn modern_problem(s: windows::Media::SpeechRecognition::SpeechRecognitionResultStatus) -> Option<String> {
    use windows::Media::SpeechRecognition::SpeechRecognitionResultStatus as S;
    match s {
        S::Success | S::UserCanceled | S::TimeoutExceeded | S::PauseLimitExceeded => None,
        S::MicrophoneUnavailable => Some("No microphone, or Windows does not let PulseTerm use it (Settings > Privacy & security > Microphone).".into()),
        S::NetworkFailure => Some("The modern speech engine needs the internet. Check the connection, or set voice_engine = \"offline\".".into()),
        S::AudioQualityFailure => Some("The mic sound was too quiet or too noisy to understand.".into()),
        other => Some(format!("Listening stopped (speech status {}).", other.0)),
    }
}

/// Wait until the mic is clicked again (or the engine stops by itself), then stop.
fn modern_wait(m: ModernRun, stop: &Receiver<()>) -> Result<(), String> {
    // Both the window's stop and the engine's "finished" handler send here.
    let _ = stop.recv();
    if !m.done.load(Ordering::SeqCst) {
        // StopAsync (not Cancel) still delivers the words of a sentence you were saying.
        if let Ok(a) = m.session.StopAsync() {
            let _ = a.join();
        }
    }
    let _ = m.rec.Close();
    let err = m.error.lock().unwrap().take();
    err.map_or(Ok(()), Err)
}

// ---------------- 2. the offline engine (SAPI 5) ----------------

/// SAPI's SPFEI(): the bit for one event, plus two bits it always wants.
fn spfei(e: SPEVENTENUM) -> u64 {
    (1u64 << e.0) | (1u64 << SPEI_RESERVED1.0) | (1u64 << SPEI_RESERVED2.0)
}

/// Windows' default object of a kind (the default recognizer, the default mic).
unsafe fn default_token(category: PCWSTR) -> windows::core::Result<ISpObjectToken> {
    let cat: ISpObjectTokenCategory = CoCreateInstance(&SpObjectTokenCategory, None, CLSCTX_ALL)?;
    cat.SetId(category, false)?;
    let id = cat.GetDefaultTokenId()?;
    let token: ISpObjectToken = CoCreateInstance(&SpObjectToken, None, CLSCTX_ALL)?;
    let r = token.SetId(PCWSTR::null(), PCWSTR(id.0), false);
    CoTaskMemFree(Some(id.0 as *const c_void));
    r?;
    Ok(token)
}

/// Take what an event carries: an object comes back (we own it), memory is freed.
unsafe fn take_lparam(ev: &SPEVENT) -> Option<IUnknown> {
    let p = ev.lParam.0 as *mut c_void;
    if p.is_null() {
        return None;
    }
    match SPEVENTLPARAMTYPE((ev._bitfield >> 16) & 0xFFFF) {
        SPET_LPARAM_IS_OBJECT | SPET_LPARAM_IS_TOKEN => Some(IUnknown::from_raw(p)),
        SPET_LPARAM_IS_POINTER | SPET_LPARAM_IS_STRING => {
            CoTaskMemFree(Some(p as *const c_void));
            None
        }
        _ => None,
    }
}

/// The words of a recognition result.
unsafe fn phrase_text(obj: &IUnknown) -> Option<String> {
    let r: ISpRecoResult = obj.cast().ok()?;
    let mut p = PWSTR::null();
    // u32::MAX = SP_GETWHOLEPHRASE: all the words.
    r.GetText(u32::MAX, u32::MAX, true, &mut p, None).ok()?;
    if p.is_null() {
        return None;
    }
    let s = p.to_string().ok();
    CoTaskMemFree(Some(p.0 as *const c_void));
    s
}

unsafe fn offline_run(session: u64, wav: Option<&str>, out: &Out, stop: &Receiver<()>, note: Option<String>, t0: Instant) -> windows::core::Result<()> {
    let reco: ISpRecognizer = CoCreateInstance(&SpInprocRecognizer, None, CLSCTX_ALL)?;
    reco.SetRecognizer(&default_token(SPCAT_RECOGNIZERS)?)?;
    match wav {
        Some(path) => {
            let s: ISpStream = CoCreateInstance(&SpStream, None, CLSCTX_ALL)?;
            s.BindToFile(&HSTRING::from(path), SPFM_OPEN_READONLY, None, None, 0)?;
            reco.SetInput(&s, true)?;
        }
        None => reco.SetInput(&default_token(SPCAT_AUDIOIN)?, true)?,
    }
    let ctx = reco.CreateRecoContext()?;
    let wanted = [SPEI_RECOGNITION, SPEI_HYPOTHESIS, SPEI_FALSE_RECOGNITION, SPEI_SOUND_START, SPEI_SOUND_END, SPEI_END_SR_STREAM];
    let interest = wanted.iter().fold(0u64, |m, e| m | spfei(*e));
    ctx.SetInterest(interest, interest)?;
    ctx.SetNotifyWin32Event()?;
    let grammar = ctx.CreateGrammar(0)?;
    grammar.LoadDictation(PCWSTR::null(), SPLO_STATIC)?;
    grammar.SetDictationState(SPRS_ACTIVE)?;
    crate::log(&format!("voice: offline engine listening after {} ms", t0.elapsed().as_millis()));
    out.send(VoiceEvent::Started { session, engine: Engine::Offline, note });

    let mut last = Instant::now();
    let mut in_sound = false;
    // Words heard but not final yet: worth waiting for before we stop.
    let mut pending = false;
    let mut stop_at: Option<Instant> = None;
    loop {
        let _ = ctx.WaitForNotifyEvent(100);
        let mut ended = false;
        loop {
            let mut ev = SPEVENT::default();
            let mut n = 0u32;
            if ctx.GetEvents(1, &mut ev, &mut n).is_err() || n == 0 {
                break;
            }
            let obj = take_lparam(&ev);
            match SPEVENTENUM(ev._bitfield & 0xFFFF) {
                SPEI_SOUND_START => {
                    in_sound = true;
                    last = Instant::now();
                }
                SPEI_SOUND_END => {
                    in_sound = false;
                    last = Instant::now();
                }
                SPEI_HYPOTHESIS => {
                    if let Some(t) = obj.as_ref().and_then(|o| phrase_text(o)) {
                        pending = true;
                        last = Instant::now();
                        out.send(VoiceEvent::Partial { session, text: t });
                    }
                }
                SPEI_RECOGNITION => {
                    pending = false;
                    last = Instant::now();
                    match obj.as_ref().and_then(|o| phrase_text(o)) {
                        Some(t) if !t.trim().is_empty() => out.send(VoiceEvent::Heard { session, text: t }),
                        _ => out.send(VoiceEvent::Partial { session, text: String::new() }),
                    }
                }
                SPEI_FALSE_RECOGNITION => {
                    pending = false;
                    out.send(VoiceEvent::Partial { session, text: String::new() });
                }
                SPEI_END_SR_STREAM => ended = true,
                _ => {}
            }
        }
        if ended {
            break;
        }
        if stop_at.is_none() && matches!(stop.try_recv(), Ok(()) | Err(TryRecvError::Disconnected)) {
            stop_at = Some(Instant::now());
        }
        match stop_at {
            // Stop asked: give a sentence in progress a moment to finish.
            Some(t) if !(pending || in_sound) || t.elapsed() >= FINISH => break,
            Some(_) => {}
            None if !in_sound && !pending && last.elapsed() >= SILENCE => break,
            None => {}
        }
    }
    let _ = grammar.SetDictationState(SPRS_INACTIVE);
    let _ = reco.SetRecoState(SPRST_INACTIVE);
    Ok(())
}

// ---------------- the window side ----------------

impl App {
    /// The mic button and Ctrl+Shift+M: start listening, or stop.
    pub fn toggle_voice(&mut self) {
        if self.voice.is_some() {
            self.stop_voice();
            return;
        }
        let Some(id) = self.focused_id() else { return };
        let mode = Mode::parse(&self.cfg.voice_engine);
        if mode == Mode::WinH {
            press_win_h();
            return;
        }
        self.guess_clear();
        self.voice_seq += 1;
        let wav = self.voice_test_wav.clone().filter(|_| crate::dev());
        let stop = start(self.voice_seq, mode, self.cfg.voice_language.clone(), wav, self.tx.clone());
        self.voice = Some(Voice { session: self.voice_seq, pane: id, engine: None, partial: String::new(), stopping: false, hold: None, discard: false, stop });
        self.invalidate();
    }

    /// Right Alt went down: push to talk. Listens until Right Alt comes up.
    pub fn voice_hold_down(&mut self) {
        if !self.cfg.voice_hold || self.voice.is_some() || Mode::parse(&self.cfg.voice_engine) == Mode::WinH {
            return;
        }
        if !self.focused_id().and_then(|id| self.panes.get(&id)).is_some_and(|p| p.live.is_some()) {
            return;
        }
        self.toggle_voice();
        if let Some(v) = &mut self.voice {
            v.hold = Some(Instant::now());
        }
    }

    /// Right Alt came up: stop, and type what was said (the last words still arrive).
    pub fn voice_hold_up(&mut self) {
        let Some(v) = &mut self.voice else { return };
        let Some(t) = v.hold.take() else { return };
        if t.elapsed() < TAP {
            v.discard = true;
        }
        self.stop_voice();
    }

    /// Another key while Right Alt is held: it was a shortcut (like AltGr+E for é), not
    /// talk. Stop and type nothing.
    pub fn voice_hold_cancel(&mut self) {
        let Some(v) = &mut self.voice else { return };
        if v.hold.take().is_some() {
            v.discard = true;
            self.stop_voice();
        }
    }

    /// Stop listening (the last words still arrive). Returns false when not listening.
    pub fn stop_voice(&mut self) -> bool {
        let Some(v) = &mut self.voice else { return false };
        if !v.stopping {
            v.stopping = true;
            let _ = v.stop.send(());
        }
        self.invalidate();
        true
    }

    /// Is this pane listening right now?
    pub fn voice_on(&self, id: u64) -> Option<&Voice> {
        self.voice.as_ref().filter(|v| v.pane == id)
    }

    pub fn on_voice(&mut self, ev: VoiceEvent) {
        let Some(v) = &mut self.voice else { return };
        if ev.session() != v.session {
            return;
        }
        match ev {
            VoiceEvent::Started { engine, note, .. } => {
                v.engine = Some(engine);
                if let Some(n) = note {
                    if !self.voice_note_shown {
                        self.voice_note_shown = true;
                        self.show_toast(format!("Listening with the offline engine. {n}"));
                    }
                }
            }
            VoiceEvent::Partial { text, .. } => v.partial = text,
            VoiceEvent::Heard { .. } if v.discard => v.partial.clear(),
            VoiceEvent::Heard { text, .. } => {
                v.partial.clear();
                let id = v.pane;
                self.type_heard(id, &text);
            }
            VoiceEvent::Ended { error, .. } => {
                self.voice = None;
                if let Some(e) = error {
                    self.show_toast(e);
                }
            }
        }
        self.invalidate();
    }

    /// Type heard words: into the Rich input box when it is open, else into Claude's
    /// own input line, as if typed on the keyboard.
    fn type_heard(&mut self, id: u64, text: &str) {
        let boxed = self.ui.boxes.iter().any(|b| b.id == id)
            && self.panes.get(&id).is_some_and(|p| !p.compose.passthrough && p.claude_status() != Some("asking"));
        if boxed {
            let c = &mut self.panes.get_mut(&id).unwrap().compose;
            let before = c.caret.checked_sub(1).and_then(|i| c.text.get(i)).map(|u| char::from_u32(*u as u32).unwrap_or('x'));
            if let Some(t) = phrase_to_type(text, before) {
                c.anchor = None;
                c.insert(&t);
                self.box_changed(id);
            }
            return;
        }
        let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) else {
            self.stop_voice();
            return;
        };
        // The character before the cursor: a space is needed after a word, not after "> ".
        let before = {
            let t = l.term.lock();
            let c = t.grid().cursor.point;
            c.column.0.checked_sub(1).map(|x| t.grid()[c.line][Column(x)].c)
        };
        let Some(t) = phrase_to_type(text, before) else { return };
        self.guess_clear();
        let Some(l) = self.panes.get(&id).and_then(|p| p.live.as_ref()) else { return };
        {
            let mut term = l.term.lock();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
            }
            term.selection = None;
        }
        l.write(t.into_bytes());
    }

    /// Listening state (for the pipe, to test it).
    pub fn voice_state(&self) -> serde_json::Value {
        match &self.voice {
            None => serde_json::json!({"ok": true, "listening": false, "mode": format!("{:?}", Mode::parse(&self.cfg.voice_engine))}),
            Some(v) => serde_json::json!({
                "ok": true, "listening": true, "pane": v.pane, "session": v.session,
                "engine": v.engine.map(|e| format!("{e:?}")), "partial": v.partial, "stopping": v.stopping,
                "hold": v.hold.is_some(), "discard": v.discard,
            }),
        }
    }

    /// Pretend the engine heard `text` (for the pipe: tests where the words go).
    pub fn voice_fake_heard(&mut self, text: &str) -> bool {
        let Some(v) = &self.voice else { return false };
        let session = v.session;
        self.on_voice(VoiceEvent::Heard { session, text: text.to_string() });
        true
    }
}

/// The old way: open Windows' voice typing panel by pressing Win+H.
fn press_win_h() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY};
    let key = |vk: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, time: 0, dwExtraInfo: 0 },
        },
    };
    let inputs = [key(0x5B, false), key(0x48, false), key(0x48, true), key(0x5B, true)];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_between_phrases() {
        assert_eq!(phrase_to_type("  hello world ", None).as_deref(), Some("hello world"));
        assert_eq!(phrase_to_type("Fix the bug.", Some(' ')).as_deref(), Some("Fix the bug."));
        assert_eq!(phrase_to_type("then test it", Some('.')).as_deref(), Some(" then test it"));
        // An empty cell in the terminal is a space or \0: no extra space.
        assert_eq!(phrase_to_type("go", Some('\0')).as_deref(), Some("go"));
        assert_eq!(phrase_to_type("   ", Some('x')), None);
        // Control characters never reach the terminal (no surprise Enter or Esc).
        assert_eq!(phrase_to_type("a\rb\x1bc", None).as_deref(), Some("abc"));
    }

    #[test]
    fn modes() {
        assert_eq!(Mode::parse(""), Mode::Auto);
        assert_eq!(Mode::parse("auto"), Mode::Auto);
        assert_eq!(Mode::parse("Offline"), Mode::Offline);
        assert_eq!(Mode::parse("modern"), Mode::Modern);
        assert_eq!(Mode::parse("win+h"), Mode::WinH);
    }

    #[test]
    fn keeps_the_newest_words() {
        let w = |s: &str| s.chars().count() as f32;
        assert_eq!(tail_that_fits("short", 10.0, w), "short");
        assert_eq!(tail_that_fits("one two three four", 11.0, w), "…three four");
        assert_eq!(tail_that_fits("averyveryverylongword", 5.0, w), "");
    }

    #[test]
    fn right_alt_only() {
        use windows::Win32::UI::WindowsAndMessaging::{WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP};
        let ext = 1isize << 24;
        let repeat = 1isize << 30;
        // Right Alt down (US layout: SYSKEYDOWN; UK/German AltGr: KEYDOWN) and up.
        assert_eq!(right_alt(WM_SYSKEYDOWN, 0x12, ext | 1), Some(true));
        assert_eq!(right_alt(WM_KEYDOWN, 0x12, ext | 1), Some(true));
        assert_eq!(right_alt(WM_SYSKEYUP, 0x12, ext), Some(false));
        assert_eq!(right_alt(WM_KEYUP, 0x12, ext), Some(false));
        // Held down: the repeats do not start it again.
        assert_eq!(right_alt(WM_SYSKEYDOWN, 0x12, ext | repeat), None);
        // Left Alt (no extended bit) and other keys are not it.
        assert_eq!(right_alt(WM_SYSKEYDOWN, 0x12, 1), None);
        assert_eq!(right_alt(WM_KEYDOWN, 0x11, ext), None);
    }

    #[test]
    fn event_bits() {
        // SPFEI(SPEI_RECOGNITION) from sapi.h.
        assert_eq!(spfei(SPEI_RECOGNITION), (1u64 << 38) | (1u64 << 30) | (1u64 << 33));
    }
}
