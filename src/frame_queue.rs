use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};

use ffmpeg_sys_next::{av_frame_alloc, av_frame_free, av_frame_unref, AVFrame};

const QUEUE_SIZE: usize = 20;

pub struct FrameQueue {
    slots: [*mut AVFrame; QUEUE_SIZE],
    write_idx: Cell<u32>,
    read_idx: Cell<u32>,
    count: AtomicU32,
    mutex: Mutex<()>,
    not_empty: Condvar,
    not_full: Condvar,
}

unsafe impl Send for FrameQueue {}
unsafe impl Sync for FrameQueue {}

#[allow(dead_code)]
impl FrameQueue {
    pub fn new() -> Self {
        let mut slots = [std::ptr::null_mut(); QUEUE_SIZE];
        for slot in slots.iter_mut() {
            *slot = unsafe { av_frame_alloc() };
            assert!(!slot.is_null());
        }

        Self {
            slots,
            write_idx: Cell::new(0),
            read_idx: Cell::new(0),
            count: AtomicU32::new(0),
            mutex: Mutex::new(()),
            not_empty: Condvar::new(),
            not_full: Condvar::new(),
        }
    }

    pub fn get_write_slot(&self) -> *mut AVFrame {
        let _guard = self.mutex.lock().unwrap();
        let _guard = self
            .not_full
            .wait_while(_guard, |_| {
                self.count.load(Ordering::Acquire) >= QUEUE_SIZE as u32
            })
            .unwrap();

        let idx = self.write_idx.get() as usize % QUEUE_SIZE;
        self.slots[idx]
    }

    pub fn commit_write(&self) {
        self.write_idx.set(self.write_idx.get() + 1);
        self.count.fetch_add(1, Ordering::Release);
        self.not_empty.notify_one();
    }

    pub fn get_read_slot(&self) -> *mut AVFrame {
        let _guard = self.mutex.lock().unwrap();
        let _guard = self
            .not_empty
            .wait_while(_guard, |_| self.count.load(Ordering::Acquire) == 0)
            .unwrap();

        let idx = self.read_idx.get() as usize % QUEUE_SIZE;
        self.slots[idx]
    }

    pub fn commit_read(&self) {
        let idx = self.read_idx.get() as usize % QUEUE_SIZE;
        unsafe {
            av_frame_unref(self.slots[idx]);
        }
        self.read_idx.set(self.read_idx.get() + 1);
        self.count.fetch_sub(1, Ordering::Release);
        self.not_full.notify_one();
    }

    pub fn try_get_read_slot(&self) -> Option<*mut AVFrame> {
        if self.count.load(Ordering::Acquire) == 0 {
            return None;
        }
        let idx = self.read_idx.get() as usize % QUEUE_SIZE;
        Some(self.slots[idx])
    }

    pub fn len(&self) -> u32 {
        self.count.load(Ordering::Acquire)
    }
}

impl Drop for FrameQueue {
    fn drop(&mut self) {
        unsafe {
            for i in 0..QUEUE_SIZE {
                if !self.slots[i].is_null() {
                    av_frame_free(&mut self.slots[i]);
                }
            }
        }
    }
}
