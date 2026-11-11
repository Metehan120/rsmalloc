use std::os::fd::AsFd;

use rustix::{
    fs::{Mode, OFlags, openat},
    io::read,
};

use super::numa_parser::NumberRangeList;

fn cpu_id_bound(bytes: &[u8]) -> Option<usize> {
    let mut highest = None;
    for range in NumberRangeList::new(bytes) {
        let end = range.ok()?.end;
        highest = Some(highest.map_or(end, |previous: usize| previous.max(end)));
    }
    highest?.checked_add(1)
}

unsafe extern "C" {
    fn get_nprocs_conf() -> i32;
}

pub(crate) fn configured_cpu_count() -> Option<usize> {
    let count = unsafe { get_nprocs_conf() };
    (count > 0).then_some(count as usize)
}

pub(crate) fn possible_cpu_count() -> Option<usize> {
    let fd = openat(
        rustix::fs::CWD,
        "/sys/devices/system/cpu/possible",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let mut buffer = [0u8; 4097];
    let mut len = 0;
    loop {
        if len == buffer.len() {
            return None;
        }
        let count = read(fd.as_fd(), &mut buffer[len..]).ok()?;
        if count == 0 {
            break;
        }
        len += count;
    }
    cpu_id_bound(&buffer[..len])
}

#[cfg(test)]
mod tests {
    use super::cpu_id_bound;

    #[test]
    fn parses_sparse_and_unordered_cpu_ids() {
        assert_eq!(cpu_id_bound(b"0-3,8-11\n"), Some(12));
        assert_eq!(cpu_id_bound(b"7,0-2,4"), Some(8));
        assert_eq!(cpu_id_bound(b"0"), Some(1));
    }

    #[test]
    fn rejects_invalid_or_overflowing_lists() {
        for input in [b"".as_slice(), b"0-", b"3-1", b"1,", b"1,x"] {
            assert_eq!(cpu_id_bound(input), None);
        }
        assert_eq!(cpu_id_bound(usize::MAX.to_string().as_bytes()), None);
    }
}
