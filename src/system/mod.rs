pub mod apps;
pub mod audio_brightness;
pub mod media;
pub mod recorder;
pub mod stats;

use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

pub const WAKE_TOP_BAR: u8 = 1 << 0;
pub const WAKE_HUD: u8 = 1 << 1;
pub const WAKE_DOCK: u8 = 1 << 2;
pub const WAKE_ALL: u8 = WAKE_TOP_BAR | WAKE_HUD | WAKE_DOCK;

/// Thread-safe bitmask wakeup signal backed by a Linux `eventfd` so background
/// threads only dirty the specific Wayland surfaces affected by their state.
#[derive(Clone)]
pub struct WakeSignal {
    fd: RawFd,
    mask: Arc<AtomicU8>,
}

impl WakeSignal {
    pub fn new(fd: RawFd) -> Self {
        Self {
            fd,
            mask: Arc::new(AtomicU8::new(0)),
        }
    }

    pub fn notify(&self, bits: u8) {
        self.mask.fetch_or(bits, Ordering::Release);
        if self.fd >= 0 {
            let val: u64 = 1;
            unsafe {
                libc::write(
                    self.fd,
                    (&val as *const u64).cast::<libc::c_void>(),
                    std::mem::size_of::<u64>(),
                );
            }
        }
    }

    pub fn take_mask(&self) -> u8 {
        self.mask.swap(0, Ordering::AcqRel)
    }
}
