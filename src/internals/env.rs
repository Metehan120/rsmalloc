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

pub unsafe fn get_env_f32(key: &[u8]) -> Option<f32> {
    let val = getenv_raw(key)?;
    let mut p = val;

    let mut sign = 1.0f32;

    if *p == b'-' {
        sign = -1.0;
        p = p.add(1);
    } else if *p == b'+' {
        p = p.add(1);
    }

    let mut out = 0.0f32;

    while *p >= b'0' && *p <= b'9' {
        out = out * 10.0 + (*p - b'0') as f32;
        p = p.add(1);
    }

    if *p == b'.' {
        p = p.add(1);

        let mut frac = 0.0f32;
        let mut div = 1.0f32;

        while *p >= b'0' && *p <= b'9' {
            frac = frac * 10.0 + (*p - b'0') as f32;
            div *= 10.0;
            p = p.add(1);
        }

        out += frac / div;
    }

    Some(sign * out)
}
