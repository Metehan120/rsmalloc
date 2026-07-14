use std::{
    env,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

static STARTED: AtomicBool = AtomicBool::new(false);

fn thread_inner() {
    let sleep = env::var("RS_PRINTER_SLEEP")
        .unwrap_or("1".to_string())
        .parse()
        .unwrap_or(1);

    loop {
        thread::sleep(Duration::from_secs(sleep));
        unsafe {
            crate::debug_exit_printer::print_report();
        }
    }
}

pub(crate) fn start() {
    if STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    if thread::Builder::new()
        .name("rsmalloc-debug-printer".into())
        .stack_size(128 * 1024)
        .spawn(thread_inner)
        .is_err()
    {
        STARTED.store(false, Ordering::Release);
    }
}
