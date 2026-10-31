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
    use std::{
        num::NonZero,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn trim_api_posts_coalesces_and_unparks() {
        const CHILD: &str = "RSMALLOC_TRIM_API_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // The real worker runs indefinitely, so keep it in a separate process.
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "frontend::tests::trim_api_posts_coalesces_and_unparks",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "trim API child test failed: {status}");
            return;
        }

        let allocator = RSMalloc::new_default();
        allocator.manual_init();
        let (requested, size) = crate::TRIM_MESSAGE.get_trim();
        assert!(!requested.load(Relaxed));
        assert!(!unsafe { maybe_unpark() });

        assert_eq!(
            allocator.trim(SimpleTrimSize::Bytes(NonZero::new(4096).unwrap())),
            RSTrimStatus::Requested,
        );
        assert!(requested.load(Relaxed));
        assert_eq!(size.load(Relaxed), 4096);
        assert_eq!(
            allocator.trim(SimpleTrimSize::All),
            RSTrimStatus::AlreadyRequested
        );
        assert_eq!(size.load(Relaxed), 4096);

        assert!(unsafe {
            crate::backend::background_thread::spawn(
                crate::backend::background_thread::background_reclaimer_main,
            )
        });
        let wait_for_request = || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while requested.load(Relaxed) {
                assert!(
                    Instant::now() < deadline,
                    "background worker did not consume trim request"
                );
                thread::sleep(Duration::from_millis(1));
            }
        };
        wait_for_request();

        assert_eq!(allocator.trim(SimpleTrimSize::All), RSTrimStatus::Requested);
        wait_for_request();
    }
}

#[cfg(feature = "preload")]
pub mod abi;
#[deprecated = "please use new v2 API"]
#[cfg(not(feature = "preload"))]
pub mod global_alloc;
#[cfg(not(feature = "preload"))]
pub mod global_alloc2;
