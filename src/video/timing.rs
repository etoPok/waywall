use ::tracing::debug;
use std::time::Instant;

pub struct Timing {
    start_time: Instant,
    time_base: ffmpeg_sys_next::AVRational,
    drop_threshold: i64,
    time_scale: f64,
    last_pts: i64,
    started: bool,
    paused_at: Option<Instant>,
    paused_pts: i64,
}

impl Default for Timing {
    fn default() -> Self {
        Self::new()
    }
}

impl Timing {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            time_base: ffmpeg_sys_next::AVRational { num: 1, den: 1 },
            drop_threshold: 50_000, // 50 ms
            time_scale: 0.0,
            last_pts: 0,
            started: false,
            paused_at: None,
            paused_pts: 0,
        }
    }

    pub fn configure(&mut self, time_base: ffmpeg_sys_next::AVRational, time_scale: f64) {
        if self.started {
            return;
        }

        self.time_scale = time_scale;
        self.time_base = time_base;
    }

    pub fn start_once(&mut self) {
        if self.started {
            return;
        }
        self.started = true;

        self.last_pts = 0;
        self.start_time = Instant::now();
    }

    pub fn restart(&mut self) {
        self.started = false;
        self.start_once();
    }

    /// Freeze the timeline.
    pub fn pause(&mut self) {
        if !self.started || self.paused_at.is_some() {
            return;
        }

        self.paused_at = Some(Instant::now());
        self.paused_pts = self.last_pts;
    }

    /// Resume the timeline where it was frozen.
    pub fn resume(&mut self) {
        let Some(paused_at) = self.paused_at.take() else {
            return;
        };

        self.start_time += paused_at.elapsed();
        self.last_pts = self.paused_pts;
    }

    /// True while the timeline is frozen.
    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn get_delay(&self, pts: i64) -> i64 {
        const NO_PTS_VALUE: i64 = i64::MIN;
        if pts == NO_PTS_VALUE {
            return 0;
        }

        let target_time_us = unsafe {
            ffmpeg_sys_next::av_rescale_q(
                pts,
                ffmpeg_sys_next::AVRational {
                    num: self.time_base.num,
                    den: self.time_base.den,
                },
                ffmpeg_sys_next::AVRational {
                    num: 1,
                    den: 1_000_000,
                },
            )
        };

        let elapsed_us = (self.start_time.elapsed().as_micros() as f64 * self.time_scale) as i64;
        let delay_us = target_time_us - elapsed_us;

        if delay_us < -self.drop_threshold {
            return -1;
        }
        if delay_us < 0 {
            return 0;
        }
        delay_us
    }

    pub fn update(&mut self, current_pts: i64) {
        if current_pts < self.last_pts - 100 {
            debug!(
                "Timing reset after seek (pts: {} -> {})",
                self.last_pts, current_pts
            );
            self.restart();
            return;
        }

        self.last_pts = current_pts;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing() -> Timing {
        let mut timing = Timing::new();
        timing.configure(
            ffmpeg_sys_next::AVRational {
                num: 1,
                den: 1_000_000,
            },
            1.0,
        );
        timing.start_once();
        timing.update(0);
        timing
    }

    #[test]
    fn resume_discards_the_paused_interval() {
        let mut timing = timing();

        timing.pause();
        std::thread::sleep(std::time::Duration::from_millis(50));
        timing.resume();

        // The next frame (one frame later) is due right away: the 50ms spent paused are
        // not part of the elapsed time.
        let delay = timing.get_delay(16_666);
        assert!(delay > 0, "the timeline was not frozen, delay: {delay}");
    }

    #[test]
    fn without_pausing_the_frame_is_late() {
        let timing = timing();

        std::thread::sleep(std::time::Duration::from_millis(50));

        // Without freezing the timeline, the frame is in the past and must be rendered
        // right away.
        assert!(timing.get_delay(16_666) <= 0);
    }

    #[test]
    fn pause_is_ignored_before_the_timeline_starts() {
        let mut timing = Timing::new();

        timing.pause();
        timing.resume();

        assert!(timing.get_delay(0) >= 0);
    }

    #[test]
    fn is_paused_reflects_the_timeline_state() {
        let mut timing = timing();

        assert!(!timing.is_paused());

        timing.pause();
        assert!(timing.is_paused());

        timing.resume();
        assert!(!timing.is_paused());
    }
}
