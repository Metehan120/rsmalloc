use loom::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use loom::thread;

const TAG_SHIFT: u32 = 56;
const TAG_STEP: usize = 1usize << TAG_SHIFT;
const TAG_MASK: usize = 0xffusize << TAG_SHIFT;
const PTR_MASK: usize = !TAG_MASK;

const A: usize = 0x10;
const B: usize = 0x20;
const C: usize = 0x30;
const D: usize = 0x40;

fn pack(ptr: usize, old_word: usize) -> usize {
    (ptr & PTR_MASK) | (old_word.wrapping_add(TAG_STEP) & TAG_MASK)
}

fn unpack_ptr(word: usize) -> usize {
    word & PTR_MASK
}

fn node_index(ptr: usize) -> usize {
    (ptr >> 4) - 1
}

struct Stack {
    head: AtomicUsize,
    next: [AtomicUsize; 4],
}

impl Stack {
    fn new() -> Self {
        Self {
            head: AtomicUsize::new(A),
            next: [
                AtomicUsize::new(B),
                AtomicUsize::new(C),
                AtomicUsize::new(0),
                AtomicUsize::new(0),
            ],
        }
    }

    fn next(&self, ptr: usize) -> &AtomicUsize {
        &self.next[node_index(ptr)]
    }

    fn pop(&self) -> usize {
        loop {
            let old = self.head.load(Ordering::Acquire);
            let head = unpack_ptr(old);
            assert_ne!(head, 0);
            let next = self.next(head).load(Ordering::Relaxed);

            if self
                .head
                .compare_exchange(old, pack(next, old), Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return head;
            }

            thread::yield_now();
        }
    }

    fn push(&self, node: usize) {
        loop {
            let old = self.head.load(Ordering::Relaxed);
            self.next(node).store(unpack_ptr(old), Ordering::Relaxed);

            if self
                .head
                .compare_exchange(old, pack(node, old), Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }

            thread::yield_now();
        }
    }
}

#[test]
// This expected panic documents the accepted eight-bit tag-wrap boundary.
// Once node reuse is protected, remove should_panic and require the stale CAS to fail.
#[should_panic(expected = "stale transfer pop CAS succeeded after tag wrap")]
fn transfer_tag_wrap_admits_stale_successor() {
    let mut builder = loom::model::Builder::new();
    builder.max_branches = 4096;
    builder.check(|| {
        let stack = Arc::new(Stack::new());
        let (loaded_tx, loaded_rx) = mpsc::channel();
        let (mutated_tx, mutated_rx) = mpsc::channel();

        let stale_stack = Arc::clone(&stack);
        let stale = thread::spawn(move || {
            let old = stale_stack.head.load(Ordering::Acquire);
            assert_eq!(unpack_ptr(old), A);
            let stale_next = stale_stack.next(A).load(Ordering::Relaxed);
            assert_eq!(stale_next, B);
            loaded_tx.send(()).unwrap();
            mutated_rx.recv().unwrap();

            if stale_stack
                .head
                .compare_exchange(
                    old,
                    pack(stale_next, old),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                panic!("stale transfer pop CAS succeeded after tag wrap");
            }
        });

        let mutating_stack = Arc::clone(&stack);
        let mutator = thread::spawn(move || {
            loaded_rx.recv().unwrap();

            assert_eq!(mutating_stack.pop(), A); // update 1
            assert_eq!(mutating_stack.pop(), B); // update 2; B remains checked out
            mutating_stack.push(A); // update 3; A -> C
            assert_eq!(mutating_stack.pop(), A); // update 4
            mutating_stack.push(D); // update 5; D -> C
            mutating_stack.push(A); // update 6; A -> D

            for _ in 0..125 {
                assert_eq!(mutating_stack.pop(), A);
                mutating_stack.push(A);
            }

            let wrapped = mutating_stack.head.load(Ordering::Acquire);
            assert_eq!(wrapped, A);
            assert_eq!(mutating_stack.next(A).load(Ordering::Relaxed), D);
            mutated_tx.send(()).unwrap();
        });

        stale.join().unwrap();
        mutator.join().unwrap();
    });
}
