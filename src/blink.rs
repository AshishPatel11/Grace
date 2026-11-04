//! The cursor blink, worked out from the clock.
//!
//! The old way flipped "on/off" on a timer. That felt random for two reasons:
//! typing turned the cursor on without restarting the timer (so the next "off" could
//! come 10 ms later), and Windows delivers timer messages last, so while Claude prints
//! a lot the flips came late. Now the look of the cursor is a function of time since
//! the last reset, so every frame agrees, however late it is drawn.
//!
//! The pattern, with B = Windows' blink time (530 ms by default):
//! solid for B after you type, click, or the cursor moves; then fade out, stay hidden,
//! fade in, stay shown, and repeat. Each fade is short and soft instead of a hard cut.
//!
//! Like Windows' own text boxes, it stops blinking after Windows' caret timeout
//! (5 seconds by default) without typing: it finishes the blink it is in and stays
//! solid. A fade redraws the window ~10 times a second (measured 0.8-2% of one core
//! against 0.16% with no blink), so a cursor that sits still while Claude works should
//! cost nothing.
//!
//! Between changes nothing is drawn; the timer wakes us only for the next one.

use std::time::Instant;

use crate::app::App;

/// How long one fade takes (it is shorter if the blink time is very short).
const FADE_MS: u64 = 150;
/// Time between frames during a fade: about 30 per second. Plenty for a soft fade, and
/// half the work of 60.
const FRAME_MS: u64 = 33;
/// Used when Windows does not say how long to blink.
const DEFAULT_TIMEOUT_MS: u64 = 5000;

/// Windows' text cursor blink time in ms, or 0 when the user turned blinking off.
pub fn system_blink_ms() -> u32 {
    let ms = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime() };
    if ms == 0 || ms == u32::MAX {
        0
    } else {
        ms.clamp(200, 1200)
    }
}

/// How long the cursor keeps blinking after the last key or move (Windows' caret
/// timeout, `HKCU\Control Panel\Desktop\CaretTimeout`). `u64::MAX` = blink forever.
pub fn system_blink_timeout_ms() -> u64 {
    use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCARETTIMEOUT, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut v: u32 = 0;
    let ok = unsafe { SystemParametersInfoW(SPI_GETCARETTIMEOUT, 0, Some(&mut v as *mut u32 as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
    match (ok.is_ok(), v) {
        (true, u32::MAX) => u64::MAX,
        (true, v) if v > 0 => v as u64,
        _ => DEFAULT_TIMEOUT_MS,
    }
}

/// A soft start and end (0..1 → 0..1).
fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn fade(b: u64) -> u64 {
    FADE_MS.min(b / 3)
}

/// When blinking ends for good (ms after the reset): the first moment at or after
/// `stop` when the cursor is fully shown, so it never stops while hidden.
fn blink_end(b: u64, stop: u64) -> u64 {
    if stop == u64::MAX {
        return u64::MAX;
    }
    if stop <= b {
        return b; // too short to blink at all
    }
    let p = (stop - b) % (2 * b);
    if p >= b + fade(b) {
        stop // already fully shown
    } else {
        stop - p + b + fade(b) // once the fade in that comes next is done
    }
}

/// The blink cycle alone (no end): 1 = solid, 0 = hidden.
fn cycle_alpha(t: u64, b: u64) -> f32 {
    if t < b {
        return 1.0;
    }
    let f = fade(b);
    let p = (t - b) % (2 * b);
    if p < f {
        1.0 - ease(p as f32 / f as f32)
    } else if p < b {
        0.0
    } else if p < b + f {
        ease((p - b) as f32 / f as f32)
    } else {
        1.0
    }
}

/// How visible the cursor is, `t` ms after the last reset (1 = solid, 0 = hidden).
/// `b`: blink time (0 = no blink). `stop`: blink timeout.
pub fn alpha_at(t: u64, b: u64, stop: u64) -> f32 {
    if b == 0 || t >= blink_end(b, stop) {
        return 1.0;
    }
    cycle_alpha(t, b)
}

/// When the cursor's look next changes, in ms from `t`. None = it does not change any more.
pub fn next_change(t: u64, b: u64, stop: u64) -> Option<u64> {
    if b == 0 {
        return None;
    }
    let end = blink_end(b, stop);
    if t >= end {
        return None;
    }
    let n = if t < b {
        b - t
    } else {
        let f = fade(b);
        let p = (t - b) % (2 * b);
        if p < f || (b..b + f).contains(&p) {
            FRAME_MS
        } else if p < b {
            b - p
        } else {
            2 * b - p
        }
    };
    if t + n >= end {
        // The next change would come after the end: draw the end itself (solid), if
        // the cursor is not solid already.
        return if cycle_alpha(t, b) >= 1.0 { None } else { Some((end - t).max(1)) };
    }
    Some(n)
}

impl App {
    /// Solid cursor now, blinking again one blink time later (you typed or clicked).
    pub fn blink_reset(&mut self) {
        self.blink_from = Instant::now();
    }

    fn blink_period(&self) -> u64 {
        if self.cfg.cursor_blink {
            self.blink_ms as u64
        } else {
            0
        }
    }

    fn blink_t(&self) -> u64 {
        self.blink_from.elapsed().as_millis() as u64
    }

    /// How visible the cursor (and the typing box caret) is right now: 0..1.
    pub fn cursor_alpha(&self) -> f32 {
        alpha_at(self.blink_t(), self.blink_period(), self.blink_timeout)
    }

    /// After a paint: wake up for the blink's next step, if a blinking cursor is on screen.
    pub fn schedule_blink(&self) {
        use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
        let next = if self.blink_needed && self.focused {
            next_change(self.blink_t(), self.blink_period(), self.blink_timeout)
        } else {
            None
        };
        unsafe {
            match next {
                // A timer with the same id replaces the old one.
                Some(ms) => {
                    SetTimer(Some(self.hwnd), crate::TIMER_BLINK, ms.clamp(1, 60_000) as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), crate::TIMER_BLINK);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: u64 = 530;
    const NEVER: u64 = u64::MAX;

    #[test]
    fn solid_after_a_reset_then_a_steady_rhythm() {
        // Solid for one blink time after typing.
        assert_eq!(alpha_at(0, B, NEVER), 1.0);
        assert_eq!(alpha_at(529, B, NEVER), 1.0);
        // Fading out, then hidden, fading in, then shown.
        let mid = alpha_at(530 + 75, B, NEVER);
        assert!(mid > 0.2 && mid < 0.8, "{mid}");
        assert_eq!(alpha_at(530 + 300, B, NEVER), 0.0);
        assert_eq!(alpha_at(530 + 530 + 300, B, NEVER), 1.0);
        // The same look every cycle (no drift).
        for k in 0..20u64 {
            let t = 530 + k * 1060;
            assert_eq!(alpha_at(t + 300, B, NEVER), 0.0);
            assert_eq!(alpha_at(t + 830, B, NEVER), 1.0);
        }
    }

    #[test]
    fn fades_are_smooth_steps() {
        let mut last = 1.0;
        for t in (530..=680).step_by(FRAME_MS as usize) {
            let a = alpha_at(t, B, NEVER);
            assert!(a <= last + 1e-6);
            assert!(last - a < 0.6, "jump at {t}: {last} -> {a}");
            last = a;
        }
    }

    #[test]
    fn wakes_only_when_something_changes() {
        assert_eq!(next_change(100, B, NEVER), Some(430)); // until the first fade
        assert_eq!(next_change(530 + 10, B, NEVER), Some(FRAME_MS)); // in a fade: next frame
        assert_eq!(next_change(530 + 300, B, NEVER), Some(230)); // hidden: until the fade in
        assert_eq!(next_change(530 + 700, B, NEVER), Some(360)); // shown: until the next fade out
        assert_eq!(next_change(100, 0, NEVER), None); // blinking off
        assert_eq!(alpha_at(5000, 0, NEVER), 1.0);
    }

    #[test]
    fn stops_after_the_timeout_and_never_while_hidden() {
        let stop = 5000;
        // 5000 ms is inside a cycle: (5000 - 530) % 1060 = 230, the hidden part.
        // It keeps going until the fade in after that is done, then stays solid.
        let end = blink_end(B, stop);
        assert_eq!(end, 5000 - 230 + 530 + 150);
        assert_eq!(alpha_at(5000, B, stop), 0.0);
        assert_eq!(alpha_at(end, B, stop), 1.0);
        for t in (end..end + 5000).step_by(97) {
            assert_eq!(alpha_at(t, B, stop), 1.0);
        }
        // No more wake-ups after the end; the last one lands on the end itself.
        assert_eq!(next_change(end, B, stop), None);
        assert_eq!(next_change(end + 10_000, B, stop), None);
        let last_frame = next_change(end - 20, B, stop).unwrap();
        assert!(last_frame <= 20, "{last_frame}");
        // Stopping while already shown: stops right there.
        let shown = 530 + 1060 * 3 + 800; // a fully shown moment
        assert_eq!(blink_end(B, shown), shown);
        // A shown cursor whose next fade would come after the end: no wake-up at all.
        assert_eq!(next_change(shown - 100, B, shown), None);
    }
}
