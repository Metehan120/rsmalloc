unsafe extern "C" {
    static mut environ: *mut *mut u8;
}

unsafe fn getenv_raw(key: &[u8]) -> Option<*const u8> {
    let mut env = environ;

    while !(*env).is_null() {
        let entry = *env;
        let mut p = entry;
        let mut i = 0;

        while *p != 0 && *p != b'=' {
            if i >= key.len() || *p != key[i] {
                break;
            }
            p = p.add(1);
            i += 1;
        }

        if i == key.len() && *p == b'=' {
            return Some(p.add(1));
        }

        env = env.add(1);
    }

    None
}

pub unsafe fn get_env_usize(key: &[u8]) -> Option<usize> {
    let val = getenv_raw(key)?;
    let mut out = 0usize;
    let mut p = val;

    while *p >= b'0' && *p <= b'9' {
        out = out * 10 + (*p - b'0') as usize;
        p = p.add(1);
    }

    Some(out)
}
