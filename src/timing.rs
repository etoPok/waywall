use std::time::{Duration, Instant};

pub struct Timing {
    start_time: Instant,
    time_base: f64,
    drop_threshold: Duration,
}

impl Timing {
    pub fn new(time_base: f64) -> Self {
        Self {
            start_time: Instant::now(),
            time_base,
            drop_threshold: Duration::from_millis(50),
        }
    }

    pub fn render_time(&self, pts: i64) -> Instant {
        const AV_NOPTS_VALUE: i64 = 0x8000000000000000u64 as i64;
        if pts == AV_NOPTS_VALUE {
            return Instant::now();
        }
        let secs = pts as f64 * self.time_base;
        if !secs.is_finite() || secs < 0.0 {
            return self.start_time;
        }
        self.start_time + Duration::from_secs_f64(secs)
    }

    pub fn should_drop(&self, pts: i64, now: Instant) -> bool {
        const AV_NOPTS_VALUE: i64 = 0x8000000000000000u64 as i64;
        if pts == AV_NOPTS_VALUE {
            return false;
        }
        let render_time = self.render_time(pts);
        now > render_time + self.drop_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn render_time_nop_returns_now() {
        let t = Timing::new(1.0 / 30.0);
        const NOP: i64 = 0x8000000000000000u64 as i64;
        let before = Instant::now();
        let rt = t.render_time(NOP);
        let after = Instant::now();
        assert!(rt >= before && rt <= after);
    }

    #[test]
    fn should_not_drop_nop() {
        let t = Timing::new(1.0 / 30.0);
        const NOP: i64 = 0x8000000000000000u64 as i64;
        assert!(!t.should_drop(NOP, Instant::now()));
    }

    #[test]
    fn should_drop_late_frame() {
        let t = Timing::new(1.0 / 1000.0);
        // pts 0 => render_time ~ start_time, now far in future => should drop
        let late = Instant::now() + Duration::from_millis(200);
        assert!(t.should_drop(0, late));
    }

    #[test]
    fn render_time_negative_secs_falls_back_to_start() {
        let t = Timing::new(1.0 / 30.0);
        // negative pts => secs negative => fallback to start_time
        let rt = t.render_time(-1000);
        // should be approximately start_time, not panic
        assert!(rt <= Instant::now());
    }
}
