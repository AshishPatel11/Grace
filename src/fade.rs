//! Hover fades: the thing under the mouse eases its highlight in, and the one it left
//! eases it out, instead of both snapping. One pair at a time (the mouse is in one place),
//! so it costs two numbers and a few extra frames only while a fade runs.

use std::time::Instant;

/// How long a hover highlight takes to fade in or out (impeccable: 150-250 ms for state).
const FADE_MS: f32 = 160.0;

pub struct HoverFade {
    /// What the mouse is over now (0 = nothing) and since when.
    key: u64,
    since: Instant,
    /// What it was over before, and when it left.
    prev: u64,
    left: Instant,
}

impl Default for HoverFade {
    fn default() -> Self {
        let now = Instant::now();
        HoverFade { key: 0, since: now, prev: 0, left: now }
    }
}

/// 0 -> 1, fast first then slow (cubic ease-out).
fn ease(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

impl HoverFade {
    /// The mouse is now over `key` (0 = nothing).
    pub fn set(&mut self, key: u64) {
        if key == self.key {
            return;
        }
        let now = Instant::now();
        // Leaving something half faded in: fade out from where it got to.
        let reached = self.amount(self.key);
        self.prev = self.key;
        self.left = now - std::time::Duration::from_secs_f32((1.0 - reached) * FADE_MS / 1000.0);
        self.key = key;
        self.since = now;
    }

    /// How much of the highlight `key` shows now, 0..1.
    pub fn amount(&self, key: u64) -> f32 {
        if key == 0 {
            return 0.0;
        }
        if key == self.key {
            return ease(self.since.elapsed().as_secs_f32() * 1000.0 / FADE_MS);
        }
        if key == self.prev {
            return 1.0 - ease(self.left.elapsed().as_secs_f32() * 1000.0 / FADE_MS);
        }
        0.0
    }

    /// A fade is still running (draw again soon).
    pub fn busy(&self) -> bool {
        let ms = |t: Instant| t.elapsed().as_secs_f32() * 1000.0;
        (self.key != 0 && ms(self.since) < FADE_MS) || (self.prev != 0 && ms(self.left) < FADE_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_in_and_out() {
        let mut f = HoverFade::default();
        assert_eq!(f.amount(5), 0.0);
        f.set(5);
        assert!(f.amount(5) < 0.2 && f.busy());
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!((f.amount(5) - 1.0).abs() < 1e-6 && !f.busy());
        f.set(7);
        assert!(f.amount(5) > 0.8, "the old one fades out, it does not vanish");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(f.amount(5), 0.0);
        assert!((f.amount(7) - 1.0).abs() < 1e-6);
    }
}
