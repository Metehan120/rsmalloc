//! Bounded correctness checks, not benchmarks. The harness and bookkeeping use
//! the system allocator; only the explicitly owned blocks below use RSMalloc.
#![cfg(not(feature = "preload"))]

use rsmalloc::v2::alloc::{RSMalloc, RawInterface};
use std::{
    collections::BTreeMap,
    env,
    sync::{Arc, Mutex, mpsc},
    thread,
};

static ALLOCATOR: RSMalloc = RSMalloc::new_default();
const WORKERS: usize = 4;
const SLOTS: usize = 24;
const SIZES: &[usize] = &[
    1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257, 1023, 1024, 1025, 4095,
    4096, 4097, 8191, 8192, 8193, 16383, 16384, 16385, 65535, 65536, 65537, 262145,
];

type Live = Arc<Mutex<BTreeMap<usize, (usize, u64)>>>;

fn rounds() -> usize {
    // This override controls rounds, not simultaneously retained allocations.
    env::var("RS_TEST_LOOP")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8)
        .clamp(1, 64)
}

fn next(seed: &mut u64) -> usize {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed as usize
}

fn pattern(tag: u64, offset: usize) -> u8 {
    (tag as u8)
        .wrapping_mul(73)
        .wrapping_add((offset as u8).wrapping_mul(29))
        ^ ((offset >> 8) as u8)
}

fn register(live: &mut BTreeMap<usize, (usize, u64)>, ptr: *mut u8, size: usize, tag: u64) {
    assert!(!ptr.is_null(), "allocation failed: tag={tag}, size={size}");
    let start = ptr as usize;
    let end = start.checked_add(size).expect("allocation range overflow");
    if let Some((&previous, &(previous_end, previous_tag))) = live.range(..=start).next_back() {
        assert!(
            previous_end <= start,
            "overlap: tag={tag} [{start:#x}, {end:#x}) with tag={previous_tag} [{previous:#x}, {previous_end:#x})"
        );
    }
    if let Some((&following, &(following_end, following_tag))) = live.range(start..).next() {
        assert!(
            end <= following,
            "overlap: tag={tag} [{start:#x}, {end:#x}) with tag={following_tag} [{following:#x}, {following_end:#x})"
        );
    }
    assert!(live.insert(start, (end, tag)).is_none());
}

struct Block {
    ptr: *mut u8,
    size: usize,
    tag: u64,
    live: Live,
}

// A Block has one owner, transferred by join/channel, and is never shared for
// concurrent payload access. The registry contains addresses, not payloads.
unsafe impl Send for Block {}

impl Block {
    fn new(size: usize, tag: u64, live: &Live) -> Self {
        let ptr = unsafe { ALLOCATOR.raw().rs_alloc(size) };
        register(&mut live.lock().unwrap(), ptr, size, tag);
        let block = Self {
            ptr,
            size,
            tag,
            live: Arc::clone(live),
        };
        block.initialize(0);
        block
    }

    fn initialize(&self, start: usize) {
        for offset in start..self.size {
            unsafe { self.ptr.add(offset).write(pattern(self.tag, offset)) };
        }
    }

    fn verify(&self) {
        self.verify_prefix(self.size);
    }

    fn verify_prefix(&self, size: usize) {
        for offset in 0..size {
            let actual = unsafe { self.ptr.add(offset).read() };
            assert_eq!(
                actual,
                pattern(self.tag, offset),
                "payload corruption: tag={}, size={}, offset={offset}, ptr={:p}",
                self.tag,
                self.size,
                self.ptr
            );
        }
    }

    fn resize(&mut self, new_size: usize) {
        assert!(new_size > 0);
        self.verify();
        // Keep the registry transition atomic: realloc may free the old address
        // before returning, allowing another worker to immediately reuse it.
        let mut live = self.live.lock().unwrap();
        let new_ptr = unsafe { ALLOCATOR.raw().rs_realloc(self.ptr, new_size, Some(16)) };
        assert!(
            !new_ptr.is_null(),
            "resize failed: tag={}, old={}, new={new_size}",
            self.tag,
            self.size
        );
        assert_eq!(
            live.remove(&(self.ptr as usize)),
            Some((self.ptr as usize + self.size, self.tag))
        );
        register(&mut live, new_ptr, new_size, self.tag);
        let preserved = self.size.min(new_size);
        self.ptr = new_ptr;
        self.size = new_size;
        drop(live);
        // Never read a grown tail until it has been initialized.
        self.verify_prefix(preserved);
        self.initialize(preserved);
        self.verify();
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        // Avoid a second panic during unwinding from a failed correctness check.
        let mut live = self.live.lock().unwrap_or_else(|error| error.into_inner());
        live.remove(&(self.ptr as usize));
        unsafe { ALLOCATOR.raw().rs_free(self.ptr) };
    }
}

#[test]
fn deterministic_mixed_sizes_remain_disjoint_and_intact() {
    let live = Live::default();
    let rounds = rounds();
    thread::scope(|scope| {
        let mut handles = Vec::new();
        for worker in 0..WORKERS {
            let live = &live;
            handles.push(scope.spawn(move || {
                let mut seed = 0x5eed_cafe_1234_5678 ^ (worker as u64 + 1);
                let mut blocks = Vec::with_capacity(SLOTS);
                for round in 0..rounds {
                    for slot in 0..SLOTS {
                        let size = SIZES[next(&mut seed) % SIZES.len()];
                        let tag = ((worker * rounds + round) * SLOTS + slot) as u64;
                        blocks.push(Block::new(size, tag, live));
                    }
                    for block in &blocks {
                        block.verify();
                    }
                    // Free in seeded, non-allocation order, then exercise reuse.
                    while !blocks.is_empty() {
                        let index = next(&mut seed) % blocks.len();
                        let block = blocks.swap_remove(index);
                        block.verify();
                        drop(block);
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    });
    assert!(live.lock().unwrap().is_empty());
}

#[test]
fn cross_thread_resize_and_free_preserve_initialized_bytes() {
    let live = Live::default();
    let rounds = rounds();
    let mut senders = Vec::new();
    let mut receivers = Vec::new();
    for _ in 0..WORKERS {
        let (sender, receiver) = mpsc::sync_channel::<Vec<Block>>(1);
        senders.push(sender);
        receivers.push(receiver);
    }
    thread::scope(|scope| {
        let mut handles = Vec::new();
        for (worker, receiver) in receivers.into_iter().enumerate() {
            let sender = senders[(worker + 1) % WORKERS].clone();
            let live = &live;
            handles.push(scope.spawn(move || {
                for round in 0..rounds {
                    let blocks = (0..SLOTS)
                        .map(|slot| {
                            let size = SIZES[(slot + worker + round) % SIZES.len()];
                            let tag = ((worker * rounds + round) * SLOTS + slot) as u64;
                            Block::new(size, tag, live)
                        })
                        .collect();
                    sender.send(blocks).unwrap();
                    let mut received = receiver.recv().unwrap();
                    for block in &mut received {
                        block.verify();
                        block.resize(block.size * 2 + 17);
                        block.resize((block.size / 4).max(1));
                    }
                    // Hand off once more: the final owner neither allocated nor
                    // resized these blocks (four workers form a ring).
                    sender.send(received).unwrap();
                    for block in receiver.recv().unwrap() {
                        block.verify();
                        drop(block);
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
    });
    assert!(live.lock().unwrap().is_empty());
}

#[test]
fn boundary_growth_and_shrink_preserve_prefixes() {
    let live = Live::default();
    for (tag, &size) in SIZES.iter().enumerate() {
        let mut block = Block::new(size, tag as u64, &live);
        block.resize(size + 1);
        block.resize(size * 2 + 17);
        block.resize(1);
        block.verify();
    }
    // A single bounded larger allocation, rather than a host-sized worker pool
    // retaining tens of GiB. Every requested byte is initialized and checked.
    let mut block = Block::new(2 * 1024 * 1024 + 1, 0xb16, &live);
    block.resize(3 * 1024 * 1024 + 17);
    block.resize(4096);
    drop(block);
    assert!(live.lock().unwrap().is_empty());
}
