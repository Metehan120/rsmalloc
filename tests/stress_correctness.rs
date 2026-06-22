// WARNING: THESE ARE NOT SPEED TESTS BUT RATHER CORRECTNESS TESTS
//
// DO NOT TAKE THEM AS SPEED RESULTS,
// THESE CASES ARE EXTREMELY UNLIKELY IN REAL WORKLOADS.
// REAL WORKLOADS MOSTLY DOMINATED BY
// COMPUTATIONAL OVERHEAD OF TASK ITSELF NOT MALLOC
//
// DO NOT RELY ON THESE TESTS FOR PERFORMANCE

use std::{
    collections::BTreeSet, env, hint::black_box, os::raw::c_void, ptr, thread, time::Instant,
};

unsafe extern "C" {
    #[link_name = "malloc"]
    fn c_malloc(size: usize) -> *mut c_void;
    #[link_name = "free"]
    fn c_free(ptr: *mut c_void);
    #[link_name = "realloc"]
    fn c_realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
}

#[derive(Debug, PartialEq, Eq)]
struct SendPtr(*mut c_void);

unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

#[inline]
unsafe fn malloc(size: usize) -> *mut c_void {
    unsafe { c_malloc(size) }
}

#[inline]
unsafe fn free(ptr: *mut c_void) {
    unsafe { c_free(ptr) };
}

#[inline]
unsafe fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    unsafe { c_realloc(ptr, size) }
}

fn run_multithreaded_allocations(loops_per_thread: usize, op: fn() -> *mut c_void) -> usize {
    let num_thread = thread::available_parallelism().unwrap();
    let mut handles = Vec::with_capacity(num_thread.get());

    for _ in 0..num_thread.get() {
        handles.push(thread::spawn(move || {
            let mut allocated = Vec::with_capacity(loops_per_thread);

            for _ in 0..loops_per_thread {
                let ptr = black_box(op());
                allocated.push(SendPtr(ptr));
            }

            allocated
        }));
    }

    let mut actual = Vec::new();
    for handle in handles {
        actual.extend(handle.join().unwrap());
    }

    let mut seen = BTreeSet::new();
    for ptr in actual {
        if ptr.0.is_null() {
            continue;
        }

        if !seen.insert(ptr.0 as usize) {
            panic!("duplicate pointer returned by allocator");
        }

        unsafe { free(ptr.0) };
    }

    num_thread.get()
}

#[test]
fn small_allocations_are_unique_under_multithreaded_stress() {
    eprintln!(
        "C ABI stress: small multithreaded malloc/free; validates uniqueness/free path, not benchmark performance"
    );
    let loop_needed = env::var("RS_TEST_LOOP")
        .unwrap_or("10000".to_string())
        .parse()
        .unwrap_or(10_000);

    run_multithreaded_allocations(loop_needed, || unsafe {
        let ptr = malloc(16);
        assert!(!ptr.is_null());
        ptr
    });
}

#[test]
fn big_allocations_are_unique_under_multithreaded_stress() {
    eprintln!(
        "C ABI stress: large multithreaded malloc/free; ignored by default because it creates heavy virtual-memory pressure"
    );

    // For example: 100 * 12 = 1200, 1200 * 32MiB = 38400MiB = 38.4GiB virtual allocation pressure.
    let loop_needed = env::var("RS_TEST_LOOP")
        .unwrap_or("100".to_string())
        .parse()
        .unwrap_or(100)
        .max(100);

    let start = Instant::now();
    let thread_count = run_multithreaded_allocations(loop_needed, || unsafe {
        let ptr = malloc(1024 * 1024 * 32);
        assert!(!ptr.is_null());
        ptr
    });
    let end = start.elapsed().as_nanos() as f64;
    eprintln!(
        "C ABI stress result: large allocation average ns/op/thread, including thread overhead: {}",
        (end / thread_count as f64) / loop_needed as f64
    );
}

#[test]
fn realloc_growth_is_unique_under_multithreaded_stress() {
    eprintln!(
        "C ABI stress: multithreaded realloc growth; validates uniqueness/free path, not benchmark performance"
    );
    let loop_needed = env::var("RS_TEST_LOOP")
        .unwrap_or("10000".to_string())
        .parse()
        .unwrap_or(10_000);

    let start = Instant::now();
    let thread_count = run_multithreaded_allocations(loop_needed, || unsafe {
        let ptr = malloc(1024 * 8);
        assert!(!ptr.is_null());
        let new_ptr = realloc(ptr, 1024 * 16);
        assert!(!new_ptr.is_null());
        new_ptr
    });
    let end = start.elapsed().as_nanos() as f64;
    eprintln!(
        "C ABI stress result: realloc growth average ns/op/thread, including thread overhead: {}",
        (end / thread_count as f64) / loop_needed as f64
    );
}

#[test]
fn realloc_shrink_is_unique_under_multithreaded_stress() {
    eprintln!(
        "C ABI stress: multithreaded realloc shrink; validates uniqueness/free path, not benchmark performance"
    );
    let loop_needed = env::var("RS_TEST_LOOP")
        .unwrap_or("10000".to_string())
        .parse()
        .unwrap_or(10_000);

    let start = Instant::now();
    let thread_count = run_multithreaded_allocations(loop_needed, || unsafe {
        let ptr = malloc(1024 * 16);
        assert!(!ptr.is_null());
        let new_ptr = realloc(ptr, 1024 * 8);
        assert!(!new_ptr.is_null());
        new_ptr
    });
    let end = start.elapsed().as_nanos() as f64;
    eprintln!(
        "C ABI stress result: realloc shrink average ns/op/thread, including thread overhead: {}",
        (end / thread_count as f64) / loop_needed as f64
    );
}

#[test]
fn realloc_slab_growth_and_shrink_preserves_prefix() {
    unsafe {
        for _ in 0..1000 {
            let ptr = malloc(4096);
            assert!(!ptr.is_null());
            ptr::write_bytes(ptr as *mut u8, 0xA5, 4096);

            let ptr = realloc(ptr, 8192);
            assert!(!ptr.is_null());
            for i in 0..4096 {
                let byte = *(ptr as *const u8).add(i);
                assert_eq!(byte, 0xA5);
            }
            ptr::write_bytes((ptr as *mut u8).add(4096), 0x5A, 4096);

            let ptr = realloc(ptr, 4096);
            assert!(!ptr.is_null());
            for i in 0..4096 {
                let byte = *(ptr as *const u8).add(i);
                assert_eq!(byte, 0xA5);
            }
            free(ptr);
        }
    }
}
