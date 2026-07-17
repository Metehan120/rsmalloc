use core::{iter::FusedIterator, mem::size_of, ptr::null_mut};
use std::os::fd::AsFd;

use crate::record_mmap_call;

use rustix::{
    fs::{Mode, OFlags, openat},
    io::read,
    mm::{MapFlags, ProtFlags, mmap_anonymous},
};

pub const INVALID_NODE: u16 = u16::MAX;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NumaCpuRange {
    pub node_id: u16,
    pub start_cpu: usize,
    pub end_cpu: usize,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NumaTopology {
    pub cpu_to_node: *mut u16,
    pub ncpu: usize,
    pub node_ids: *mut u16,
    pub nnodes: usize,
    pub cpu_ranges: *mut NumaCpuRange,
    pub nranges: usize,
}

unsafe fn read_file_stack(path: &str, buf: &mut [u8]) -> Option<usize> {
    let fd = openat(
        rustix::fs::CWD,
        path,
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;

    read(fd.as_fd(), buf).ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NumberRange {
    start: usize,
    end: usize,
}

impl NumberRange {
    fn new(start: usize, end: usize) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    fn iter(self) -> NumberRangeIter {
        NumberRangeIter {
            next: self.start,
            end: self.end,
            finished: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NumberRangeIter {
    next: usize,
    end: usize,
    finished: bool,
}

impl Iterator for NumberRangeIter {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        let item = self.next;
        self.finished = item == self.end;

        if !self.finished {
            self.next += 1;
        }

        Some(item)
    }
}

impl FusedIterator for NumberRangeIter {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParseListError;

struct NumberRangeList<'a> {
    bytes: &'a [u8],
    index: usize,
    finished: bool,
}

impl<'a> NumberRangeList<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            index: 0,
            finished: false,
        }
    }

    fn skip_ascii_whitespace(&mut self) {
        while self
            .bytes
            .get(self.index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.index += 1;
        }
    }

    fn parse_usize(&mut self) -> Option<usize> {
        let mut n = 0usize;
        let mut parsed_digit = false;

        while let Some(digit) = self
            .bytes
            .get(self.index)
            .and_then(|b| b.is_ascii_digit().then(|| b - b'0'))
        {
            n = n.checked_mul(10)?.checked_add(digit as usize)?;
            self.index += 1;
            parsed_digit = true;
        }

        parsed_digit.then_some(n)
    }

    fn parse_range(&mut self) -> Result<Option<NumberRange>, ParseListError> {
        self.skip_ascii_whitespace();

        if self.index >= self.bytes.len() {
            return Ok(None);
        }

        let start = self.parse_usize().ok_or(ParseListError)?;
        self.skip_ascii_whitespace();

        let end = if self.bytes.get(self.index) == Some(&b'-') {
            self.index += 1;
            self.skip_ascii_whitespace();
            self.parse_usize().ok_or(ParseListError)?
        } else {
            start
        };

        self.skip_ascii_whitespace();

        match self.bytes.get(self.index) {
            Some(b',') => {
                self.index += 1;
                self.skip_ascii_whitespace();

                if self.index >= self.bytes.len() {
                    return Err(ParseListError);
                }
            }
            Some(_) => return Err(ParseListError),
            None => {}
        }

        NumberRange::new(start, end).ok_or(ParseListError).map(Some)
    }
}

impl Iterator for NumberRangeList<'_> {
    type Item = Result<NumberRange, ParseListError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        match self.parse_range() {
            Ok(Some(range)) => Some(Ok(range)),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(err) => {
                self.finished = true;
                Some(Err(err))
            }
        }
    }
}

impl FusedIterator for NumberRangeList<'_> {}

struct NumberList<'a> {
    ranges: NumberRangeList<'a>,
    pending: Option<NumberRangeIter>,
}

impl<'a> NumberList<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            ranges: NumberRangeList::new(bytes),
            pending: None,
        }
    }
}

impl Iterator for NumberList<'_> {
    type Item = Result<usize, ParseListError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(range) = &mut self.pending {
            if let Some(item) = range.next() {
                return Some(Ok(item));
            }
        }

        self.pending = None;

        match self.ranges.next()? {
            Ok(range) => {
                let mut range = range.iter();
                let item = range.next().expect("range is never empty");
                self.pending = Some(range);
                Some(Ok(item))
            }
            Err(err) => Some(Err(err)),
        }
    }
}

impl FusedIterator for NumberList<'_> {}

fn parse_number_ranges(bytes: &[u8]) -> NumberRangeList<'_> {
    NumberRangeList::new(bytes)
}

fn parse_number_list(bytes: &[u8]) -> NumberList<'_> {
    NumberList::new(bytes)
}

unsafe fn mmap_array<T>(count: usize) -> Option<*mut T> {
    if count == 0 {
        return Some(null_mut());
    }

    let bytes = count.checked_mul(size_of::<T>())?;
    record_mmap_call(bytes);

    let ptr = mmap_anonymous(
        null_mut(),
        bytes,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::PRIVATE,
    )
    .ok()?;

    Some(ptr as *mut T)
}

unsafe fn init_cpu_to_node(cpu_to_node: *mut u16, ncpu: usize, node: u16) {
    for cpu in 0..ncpu {
        *cpu_to_node.add(cpu) = node;
    }
}

fn clipped_cpu_range(range: NumberRange, ncpu: usize) -> Option<NumberRange> {
    if ncpu == 0 || range.start >= ncpu {
        return None;
    }

    Some(NumberRange {
        start: range.start,
        end: range.end.min(ncpu - 1),
    })
}

unsafe fn populate_cpu_ranges(
    cpu_to_node: *mut u16,
    ncpu: usize,
    cpu_ranges: *mut NumaCpuRange,
    nranges: usize,
) {
    for cpu in 0..ncpu {
        if *cpu_to_node.add(cpu) == INVALID_NODE || *cpu_to_node.add(cpu) as usize >= nranges {
            *cpu_to_node.add(cpu) = 0;
        }

        let node = *cpu_to_node.add(cpu) as usize;
        let range = &mut *cpu_ranges.add(node);

        if range.start_cpu == usize::MAX {
            range.start_cpu = cpu;
        }

        range.end_cpu = cpu;
    }

    for node in 0..nranges {
        let range = &mut *cpu_ranges.add(node);

        if range.start_cpu == usize::MAX {
            range.start_cpu = 1;
            range.end_cpu = 0;
        }
    }
}

unsafe fn single_node_topology(ncpu: usize) -> Option<NumaTopology> {
    let cpu_to_node = mmap_array::<u16>(ncpu)?;
    init_cpu_to_node(cpu_to_node, ncpu, 0);

    let node_ids = mmap_array::<u16>(1)?;
    *node_ids = 0;

    let nranges = 1;
    let cpu_ranges = mmap_array::<NumaCpuRange>(nranges)?;
    *cpu_ranges = NumaCpuRange {
        node_id: 0,
        start_cpu: 0,
        end_cpu: ncpu.saturating_sub(1),
    };

    Some(NumaTopology {
        cpu_to_node,
        ncpu,
        node_ids,
        nnodes: 1,
        cpu_ranges,
        nranges,
    })
}

pub unsafe fn parse_numa_topology(ncpu: usize) -> Option<NumaTopology> {
    let mut buf = [0u8; 4096];

    let Some(online_len) = read_file_stack("/sys/devices/system/node/online", &mut buf) else {
        return single_node_topology(ncpu);
    };

    let online_nodes = &buf[..online_len];
    let mut nnodes = 0usize;

    for node in parse_number_list(online_nodes) {
        let node = node.ok()?;

        if u16::try_from(node).is_ok() {
            nnodes += 1;
        }
    }

    if nnodes == 0 {
        return single_node_topology(ncpu);
    }

    let cpu_to_node = mmap_array::<u16>(ncpu)?;
    init_cpu_to_node(cpu_to_node, ncpu, INVALID_NODE);

    let node_ids = mmap_array::<u16>(nnodes)?;
    let mut node_index = 0usize;
    let mut max_node = 0u16;

    for node in parse_number_list(online_nodes) {
        let node = node.ok()?;
        let Ok(node) = u16::try_from(node) else {
            continue;
        };

        *node_ids.add(node_index) = node;
        node_index += 1;
        max_node = max_node.max(node);
    }

    let nranges = max_node as usize + 1;
    let cpu_ranges = mmap_array::<NumaCpuRange>(nranges)?;

    for node in 0..nranges {
        *cpu_ranges.add(node) = NumaCpuRange {
            node_id: node as u16,
            start_cpu: usize::MAX,
            end_cpu: 0,
        };
    }

    for i in 0..nnodes {
        let node = *node_ids.add(i);

        let mut path_buf = [0u8; 128];
        let path = format_node_cpulist_path(node as usize, &mut path_buf)?;

        let len = read_file_stack(path, &mut buf)?;

        for range in parse_number_ranges(&buf[..len]) {
            let Some(range) = clipped_cpu_range(range.ok()?, ncpu) else {
                continue;
            };

            for cpu in range.iter() {
                *cpu_to_node.add(cpu) = node;
            }
        }
    }

    populate_cpu_ranges(cpu_to_node, ncpu, cpu_ranges, nranges);

    Some(NumaTopology {
        cpu_to_node,
        ncpu,
        node_ids,
        nnodes,
        cpu_ranges,
        nranges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[u8]) -> Option<Vec<usize>> {
        parse_number_list(input).collect::<Result<Vec<_>, _>>().ok()
    }

    fn parse_ranges(input: &[u8]) -> Option<Vec<(usize, usize)>> {
        parse_number_ranges(input)
            .map(|range| range.map(|range| (range.start, range.end)))
            .collect::<Result<Vec<_>, _>>()
            .ok()
    }

    #[test]
    fn parses_comma_separated_ranges() {
        assert_eq!(parse(b"0-3,8,10-11\n"), Some(vec![0, 1, 2, 3, 8, 10, 11]));
    }

    #[test]
    fn allows_whitespace_around_items_and_commas() {
        assert_eq!(parse(b" 0 - 2 , 4\t, 6\n"), Some(vec![0, 1, 2, 4, 6]));
    }

    #[test]
    fn parses_ranges_without_expanding() {
        assert_eq!(
            parse_ranges(b"0-3,8,10-11\n"),
            Some(vec![(0, 3), (8, 8), (10, 11)])
        );
    }

    #[test]
    fn clips_ranges_to_known_cpus() {
        assert_eq!(
            clipped_cpu_range(NumberRange { start: 2, end: 8 }, 4),
            Some(NumberRange { start: 2, end: 3 })
        );
        assert_eq!(clipped_cpu_range(NumberRange { start: 4, end: 8 }, 4), None);
        assert_eq!(clipped_cpu_range(NumberRange { start: 0, end: 0 }, 0), None);
    }

    #[test]
    fn rejects_malformed_lists() {
        assert_eq!(parse(b"0-"), None);
        assert_eq!(parse(b"3-1"), None);
        assert_eq!(parse(b"0,foo"), None);
        assert_eq!(parse(b"0,"), None);
        assert_eq!(parse(b"0 1"), None);
    }

    #[test]
    fn rejects_overflowing_numbers() {
        assert_eq!(parse(b"18446744073709551616"), None);
    }

    #[test]
    fn populates_cpu_ranges_by_node_id() {
        let mut cpu_to_node = [0, 0, 1, 1, 0, 2];
        let mut cpu_ranges = [NumaCpuRange {
            node_id: 0,
            start_cpu: usize::MAX,
            end_cpu: 0,
        }; 3];

        for (node, range) in cpu_ranges.iter_mut().enumerate() {
            range.node_id = node as u16;
        }

        unsafe {
            populate_cpu_ranges(
                cpu_to_node.as_mut_ptr(),
                cpu_to_node.len(),
                cpu_ranges.as_mut_ptr(),
                cpu_ranges.len(),
            );
        }

        assert_eq!(cpu_ranges[0].start_cpu, 0);
        assert_eq!(cpu_ranges[0].end_cpu, 4);
        assert_eq!(cpu_ranges[1].start_cpu, 2);
        assert_eq!(cpu_ranges[1].end_cpu, 3);
        assert_eq!(cpu_ranges[2].start_cpu, 5);
        assert_eq!(cpu_ranges[2].end_cpu, 5);
    }

    #[test]
    fn missing_and_invalid_cpus_fallback_to_node_zero() {
        let mut cpu_to_node = [INVALID_NODE, 2, INVALID_NODE, 99];
        let mut cpu_ranges = [NumaCpuRange {
            node_id: 0,
            start_cpu: usize::MAX,
            end_cpu: 0,
        }; 4];

        for (node, range) in cpu_ranges.iter_mut().enumerate() {
            range.node_id = node as u16;
        }

        unsafe {
            populate_cpu_ranges(
                cpu_to_node.as_mut_ptr(),
                cpu_to_node.len(),
                cpu_ranges.as_mut_ptr(),
                cpu_ranges.len(),
            );
        }

        assert_eq!(cpu_to_node, [0, 2, 0, 0]);
        assert_eq!(cpu_ranges[0].start_cpu, 0);
        assert_eq!(cpu_ranges[0].end_cpu, 3);
        assert_eq!(cpu_ranges[1].start_cpu, 0);
        assert_eq!(cpu_ranges[1].end_cpu, 0);
        assert_eq!(cpu_ranges[2].start_cpu, 1);
        assert_eq!(cpu_ranges[2].end_cpu, 1);
        assert_eq!(cpu_ranges[3].start_cpu, 0);
        assert_eq!(cpu_ranges[3].end_cpu, 0);
    }
}

fn format_node_cpulist_path<'a>(node: usize, buf: &'a mut [u8]) -> Option<&'a str> {
    const PREFIX: &[u8] = b"/sys/devices/system/node/node";
    const SUFFIX: &[u8] = b"/cpulist";

    let mut i = 0usize;

    for &b in PREFIX {
        *buf.get_mut(i)? = b;
        i += 1;
    }

    let mut digits = [0u8; 20];
    let mut n = node;
    let mut d = 0usize;

    if n == 0 {
        digits[d] = b'0';
        d += 1;
    } else {
        while n > 0 {
            digits[d] = b'0' + (n % 10) as u8;
            n /= 10;
            d += 1;
        }
    }

    while d > 0 {
        d -= 1;
        *buf.get_mut(i)? = digits[d];
        i += 1;
    }

    for &b in SUFFIX {
        *buf.get_mut(i)? = b;
        i += 1;
    }

    core::str::from_utf8(&buf[..i]).ok()
}
