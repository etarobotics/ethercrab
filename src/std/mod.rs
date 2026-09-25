//! Items required for running in `std` environments.

#[cfg(all(target_os = "linux", feature = "io-uring"))]
mod io_uring;
#[cfg(unix)]
mod unix;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(all(target_os = "linux", feature = "xdp"))]
mod xdp;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::{
    sync::Arc,
    task::Wake,
    thread::{self, Thread},
};

#[cfg(target_os = "windows")]
pub use self::windows::{TxRxTaskConfig, ethercat_now, tx_rx_task_blocking};
#[cfg(unix)]
pub use unix::{ethercat_now, tx_rx_task};
// io_uring is Linux-only
#[cfg(all(target_os = "linux", feature = "io-uring"))]
pub use io_uring::tx_rx_task_io_uring;
#[cfg(all(target_os = "linux", feature = "xdp"))]
pub use xdp::tx_rx_task_xdp;

// Only the io_uring (Linux) and Windows blocking TX/RX loops park a thread this way; the generic
// unix loop (e.g. macOS) does not, so it would otherwise be dead code there.
#[cfg(any(target_os = "linux", target_os = "windows"))]
struct ParkSignal {
    current_thread: Thread,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
impl ParkSignal {
    fn new() -> Self {
        Self {
            current_thread: thread::current(),
        }
    }

    fn wait(&self) {
        thread::park();
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
impl Wake for ParkSignal {
    fn wake(self: Arc<Self>) {
        self.current_thread.unpark();
    }
}
