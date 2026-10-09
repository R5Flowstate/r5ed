//! Progress lines for a bake that runs for minutes: `PROGRESS <pct> <message>`
//! on stderr, the convention every tool in the pipeline prints and the host
//! reads, at most one line per interval per stage, with an estimate of the time
//! left. Stages tick from rayon workers, so the meter is lock-free.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How often a stage reports while it runs.
const INTERVAL: Duration = Duration::from_secs(10);

/// One stage of a bake: work units done out of a total, reported as a share
/// of the build's overall `from..to` percent range.
pub struct Meter {
    stage: &'static str,
    unit: &'static str,
    total: u64,
    done: AtomicU64,
    /// Milliseconds since `start` of the last printed line.
    last: AtomicU64,
    start: Instant,
    from: u8,
    to: u8,
}

impl Meter {
    pub fn new(stage: &'static str, unit: &'static str, total: u64, from: u8, to: u8) -> Self {
        let m = Meter { stage, unit, total, done: AtomicU64::new(0), last: AtomicU64::new(0), start: Instant::now(), from, to };
        eprintln!("PROGRESS {} baking {}: {} {}", from, stage, total, unit);
        m
    }

    /// Adds `n` finished units and prints a line if the interval has passed.
    pub fn tick(&self, n: u64) {
        let done = self.done.fetch_add(n, Ordering::Relaxed) + n;
        let now = self.start.elapsed().as_millis() as u64;
        let last = self.last.load(Ordering::Relaxed);
        if now.saturating_sub(last) < INTERVAL.as_millis() as u64 {
            return;
        }
        // One worker wins the right to print this interval.
        if self.last.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            return;
        }
        eprintln!("{}", self.line(done, Duration::from_millis(now)));
    }

    /// The line for `done` units after `elapsed`.
    pub fn line(&self, done: u64, elapsed: Duration) -> String {
        let frac = if self.total == 0 { 1.0 } else { (done as f64 / self.total as f64).clamp(0.0, 1.0) };
        let pct = self.from as f64 + (self.to.saturating_sub(self.from)) as f64 * frac;
        let left = if frac > 0.0 { elapsed.as_secs_f64() * (1.0 - frac) / frac } else { f64::NAN };
        format!(
            "PROGRESS {} baking {}: {:.0}% ({}/{} {}), {} elapsed, about {} left",
            pct.floor() as u8,
            self.stage,
            frac * 100.0,
            done,
            self.total,
            self.unit,
            clock(elapsed.as_secs_f64()),
            if left.is_finite() { clock(left) } else { "?".to_string() }
        )
    }

    /// Prints the stage's closing line.
    pub fn finish(&self) {
        eprintln!("PROGRESS {} baked {} in {}", self.to, self.stage, clock(self.start.elapsed().as_secs_f64()));
    }
}

/// Seconds as `1h02m`, `12m05s` or `42s`.
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match (s / 3600, (s / 60) % 60, s % 60) {
        (0, 0, sec) => format!("{sec}s"),
        (0, m, sec) => format!("{m}m{sec:02}s"),
        (h, m, _) => format!("{h}h{m:02}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_maps_into_the_stage_range_and_estimates_the_rest() {
        let m = Meter::new("lightmaps", "texels", 1000, 20, 80);
        let l = m.line(250, Duration::from_secs(60));
        assert!(l.starts_with("PROGRESS 35 baking lightmaps: 25% (250/1000 texels)"), "{l}");
        assert!(l.ends_with("1m00s elapsed, about 3m00s left"), "{l}");
        assert!(m.line(0, Duration::from_secs(1)).ends_with("about ? left"));
        assert!(m.line(1000, Duration::from_secs(9)).starts_with("PROGRESS 80 "));
    }

    #[test]
    fn ticks_from_many_threads_add_up() {
        let m = Meter::new("probes", "probes", 4000, 0, 100);
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| (0..500).for_each(|_| m.tick(1)));
            }
        });
        assert_eq!(m.done.load(Ordering::Relaxed), 4000);
    }

    #[test]
    fn the_clock_reads_like_a_person_would() {
        assert_eq!(clock(42.0), "42s");
        assert_eq!(clock(725.0), "12m05s");
        assert_eq!(clock(3720.0), "1h02m");
    }
}
