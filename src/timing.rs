use ::tracing::debug;
use std::time::Instant;

pub struct Timing {
    start_time: Instant,
    time_base: ffmpeg_sys_next::AVRational,
    drop_threshold: i64,
    last_pts: i64,
    started: bool,
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
            last_pts: 0,
            started: false,
        }
    }

    pub fn configure(&mut self, time_base: ffmpeg_sys_next::AVRational) {
        if self.started {
            return;
        }

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

        let elapsed_us = self.start_time.elapsed().as_micros() as i64;
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
