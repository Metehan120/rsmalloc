use std::os::raw::{c_char, c_int, c_void};

use rustix::io::Errno;

unsafe extern "C" {
    pub fn __errno_location() -> *mut c_int;
    pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    pub fn pthread_atfork(
        prepare: Option<unsafe extern "C" fn()>,
        parent: Option<unsafe extern "C" fn()>,
        child: Option<unsafe extern "C" fn()>,
    ) -> c_int;
    pub fn get_nprocs_conf() -> i32;
}

pub const NOMEM: i32 = Errno::NOMEM.raw_os_error();
pub const RTLD_NEXT: *mut c_void = 0xFFFFFFFFFFFFFFFF as *mut c_void;
