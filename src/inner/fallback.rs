use std::{
    os::raw::{c_char, c_void},
    ptr::null_mut,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{
    inner::libc_int::{RTLD_NEXT, dlsym},
    internals::once::Once,
};

type FreeFn = unsafe extern "C" fn(*mut c_void);
type ReallocFn = unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void;
type MallocUsableSizeFn = unsafe extern "C" fn(*mut c_void) -> usize;

static FREE_INIT: Once = Once::new();
static FREE_PTR: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

static REALLOC_INIT: Once = Once::new();
static REALLOC_PTR: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

static MUS_INIT: Once = Once::new();
static MUS_PTR: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

static MALLOC_INIT: Once = Once::new();

pub fn fallback_reinit_on_fork() {
    FREE_INIT.reset_at_fork();
    REALLOC_INIT.reset_at_fork();
    MUS_INIT.reset_at_fork();
    MALLOC_INIT.reset_at_fork();
}

fn get_symbol(name: &[u8], init: &Once, slot: &AtomicPtr<c_void>) -> *mut c_void {
    init.call_once(|| {
        let sym = unsafe { dlsym(RTLD_NEXT, name.as_ptr() as *const c_char) };
        slot.store(sym, Ordering::Relaxed);
    });
    slot.load(Ordering::Relaxed)
}

pub unsafe fn free_fallback(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }

    let sym = get_symbol(b"free\0", &FREE_INIT, &FREE_PTR);
    if sym.is_null() {
        return;
    }

    let func: FreeFn = unsafe { std::mem::transmute(sym) };
    func(ptr);
}

pub unsafe fn realloc_fallback(ptr: *mut c_void, size: usize) -> *mut c_void {
    let sym = get_symbol(b"realloc\0", &REALLOC_INIT, &REALLOC_PTR);
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

    let sym = get_symbol(b"malloc_usable_size\0", &MUS_INIT, &MUS_PTR);
    if sym.is_null() {
        return 0;
    }

    let func: MallocUsableSizeFn = unsafe { std::mem::transmute(sym) };
    func(ptr)
}
