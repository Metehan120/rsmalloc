use std::sync::atomic::Ordering::Relaxed;

use crate::backend::background_thread::maybe_unpark;

pub(crate) fn post_trim_request(size: usize) -> bool {
    let (requested, requested_size) = crate::TRIM_MESSAGE.get_trim();
    if requested.load(Relaxed) {
        return false;
    }

    requested_size.store(size, Relaxed);
    let is_ok = !requested.swap(true, Relaxed);
    unsafe { maybe_unpark() };
    is_ok
}

#[cfg(all(test, not(feature = "preload")))]
mod tests {
    use super::*;
    use crate::v2::alloc::{RSMalloc, RSMallocCoreAPI, RSTrimStatus, SimpleTrimSize};
    use std::{num::NonZero, sync::mpsc, thread, time::Duration};

    #[test]
    fn trim_api_posts_coalesces_and_unparks() {
        let allocator = RSMalloc::new_default();
        allocator.manual_init();
        let (requested, size) = crate::TRIM_MESSAGE.get_trim();
        assert!(!requested.load(Relaxed));

        let (ready_tx, ready_rx) = mpsc::channel();
        let (wake_tx, wake_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            thread::park_timeout(Duration::from_secs(5));
            wake_tx.send(()).unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // This test runs without an allocator maintenance worker. Install a
        // parked stand-in to check the frontend's wakeup independently.
        let previous = unsafe {
            std::ptr::replace(
                std::ptr::addr_of_mut!(crate::backend::background_thread::THREAD),
                Some(worker.thread().clone()),
            )
        };
        assert!(previous.is_none());

        assert_eq!(allocator.trim(SimpleTrimSize::All), RSTrimStatus::Requested);
        let woke = wake_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        worker.thread().unpark();
        worker.join().unwrap();
        unsafe { crate::backend::background_thread::THREAD = previous };
        assert!(
            woke,
            "posting a trim request did not unpark the test worker"
        );
        assert!(requested.load(Relaxed));
        assert_eq!(size.load(Relaxed), 0);
        assert_eq!(
            allocator.trim(SimpleTrimSize::Bytes(NonZero::new(4096).unwrap())),
            RSTrimStatus::AlreadyRequested,
        );
        assert_eq!(size.load(Relaxed), 0);

        requested.store(false, Relaxed);
        assert_eq!(
            allocator.trim(SimpleTrimSize::Bytes(NonZero::new(4096).unwrap())),
            RSTrimStatus::Requested,
        );
        assert_eq!(size.load(Relaxed), 4096);

        requested.store(false, Relaxed);
        size.store(0, Relaxed);
    }
}

#[cfg(feature = "preload")]
pub mod abi;
#[deprecated = "please use new v2 API"]
#[cfg(not(feature = "preload"))]
pub mod global_alloc;
#[cfg(not(feature = "preload"))]
pub mod global_alloc2;
