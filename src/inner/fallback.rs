use std::{
    os::raw::{c_char, c_void},
    ptr::null_mut,
};

use crate::{
    inner::libc_int::{RTLD_NEXT, dlsym},
    internals::oncelock::OnceLock,
};

type FreeFn = unsafe extern "C" fn(*mut c_void);
type ReallocFn = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
type MallocUsableSizeFn = unsafe extern "C" fn(*mut c_void) -> usize;

static FREE_FALLBACK: OnceLock<*mut c_void> = OnceLock::new();
static REALLOC_FALLBACK: OnceLock<*mut c_void> = OnceLock::new();
static MUS_FALLBACK: OnceLock<*mut c_void> = OnceLock::new();

pub unsafe fn fallback_reinit_on_fork() {
    FREE_FALLBACK.reset_at_fork();
    REALLOC_FALLBACK.reset_at_fork();
    MUS_FALLBACK.reset_at_fork();
}

fn get_symbol(name: &[u8], init: &OnceLock<*mut c_void>) -> *mut c_void {
    *init.get_or_init(|| unsafe { dlsym(RTLD_NEXT, name.as_ptr() as *const c_char) })
}

pub unsafe fn free_fallback(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }

    let sym = get_symbol(b"free\0", &FREE_FALLBACK);
    if sym.is_null() {
        return;
    }

    let func: FreeFn = unsafe { std::mem::transmute(sym) };
    func(ptr);
}

pub unsafe fn realloc_fallback(ptr: *mut c_void, size: usize) -> *mut c_void {
    let sym = get_symbol(b"realloc\0", &REALLOC_FALLBACK);
    if sym.is_null() {
        return null_mut();
    }

    let func: ReallocFn = unsafe { std::mem::transmute(sym) };
    func(ptr, size)
}

pub unsafe fn malloc_usable_size_fallback(ptr: *mut c_void) -> usize {
    if ptr.is_null() {
        return 0;
    }

    let sym = get_symbol(b"malloc_usable_size\0", &MUS_FALLBACK);
    if sym.is_null() {
        return 0;
    }

    let func: MallocUsableSizeFn = unsafe { std::mem::transmute(sym) };
    func(ptr)
}
