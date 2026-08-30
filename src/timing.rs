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
