// This file is AI generated, I dont know how to make red-black tree so I had
// to use AI.
// Agent: Claude CODE.
//
// If possible rewrite this code in future...
// which is seems like we are not going to need a rewite.
//
// - Metehan

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{
    BigAllocMeta, RSMallocError, internals::lock::SpinLock, record_mmap_call, traits::Lock,
};
use std::{
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, Ordering},
};

const NODE_CHUNK: usize = 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Color {
    Red,
    Black,
}

#[repr(C)]
struct Node {
    key: usize,
    meta: BigAllocMeta,
    left: *mut Node,
    right: *mut Node,
    parent: *mut Node,
    color: Color,
}

#[inline(always)]
unsafe fn color_of(n: *mut Node) -> Color {
    if n.is_null() {
        Color::Black
    } else {
        unsafe { (*n).color }
    }
}

pub struct RBTree {
    root: AtomicPtr<Node>,
    free_list: AtomicPtr<Node>,
    lock: SpinLock<()>,
}

pub static BIG_META_MAP: RBTree = RBTree::new();
pub use BIG_META_MAP as BIG_MAP;

impl RBTree {
    pub const fn new() -> Self {
        RBTree {
            root: AtomicPtr::new(null_mut()),
            free_list: AtomicPtr::new(null_mut()),
            lock: SpinLock::new(()),
        }
    }

    pub unsafe fn insert(&self, key: usize, meta: BigAllocMeta) {
        unsafe {
            let _guard = self.lock.lock();
            self.tree_insert(key, meta);
        }
    }

    pub unsafe fn get(&self, key: usize) -> Option<BigAllocMeta> {
        unsafe {
            let _guard = self.lock.lock();
            let node = self.find_node(key);
            if node.is_null() {
                None
            } else {
                Some((*node).meta)
            }
        }
    }

    pub unsafe fn replace(&self, key: usize, meta: BigAllocMeta) -> Option<BigAllocMeta> {
        unsafe {
            let _guard = self.lock.lock();
            let node = self.find_node(key);
            if node.is_null() {
                None
            } else {
                let old = (*node).meta;
                (*node).meta = meta;
                Some(old)
            }
        }
    }

    #[cfg(feature = "preload")]
    pub fn lock_for_fork(&self) {
        core::mem::forget(self.lock.lock());
    }

    #[cfg(feature = "preload")]
    pub fn reset_lock_on_fork(&self) {
        self.lock.reset_at_fork();
    }

    pub unsafe fn remove(&self, key: usize) -> Option<BigAllocMeta> {
        let _guard = self.lock.lock();
        let node = self.find_node(key);
        if node.is_null() {
            return None;
        }
        let meta = (*node).meta;
        self.delete_node(node);
        Some(meta)
    }

    unsafe fn find_node(&self, key: usize) -> *mut Node {
        let mut x = self.root.load(Ordering::Relaxed);
        while !x.is_null() {
            if key == (*x).key {
                return x;
            }
            x = if key < (*x).key {
                (*x).left
            } else {
                (*x).right
            };
        }
        null_mut()
    }

    unsafe fn tree_insert(&self, key: usize, meta: BigAllocMeta) {
        let mut y: *mut Node = null_mut();
        let mut x = self.root.load(Ordering::Relaxed);
        while !x.is_null() {
            y = x;
            if key == (*x).key {
                (*x).meta = meta;
                return;
            } else if key < (*x).key {
                x = (*x).left;
            } else {
                x = (*x).right;
            }
        }

        let z = self.alloc_node(key, meta);
        (*z).parent = y;
        if y.is_null() {
            self.root.store(z, Ordering::Relaxed);
        } else if key < (*y).key {
            (*y).left = z;
        } else {
            (*y).right = z;
        }
        self.insert_fixup(z);
    }

    unsafe fn insert_fixup(&self, mut z: *mut Node) {
        while color_of((*z).parent) == Color::Red {
            let zp = (*z).parent;
            let zpp = (*zp).parent;
            if zp == (*zpp).left {
                let y = (*zpp).right;
                if color_of(y) == Color::Red {
                    (*zp).color = Color::Black;
                    (*y).color = Color::Black;
                    (*zpp).color = Color::Red;
                    z = zpp;
                } else {
                    if z == (*zp).right {
                        z = zp;
                        self.rotate_left(z);
                    }
                    let zp = (*z).parent;
                    let zpp = (*zp).parent;
                    (*zp).color = Color::Black;
                    (*zpp).color = Color::Red;
                    self.rotate_right(zpp);
                }
            } else {
                let y = (*zpp).left;
                if color_of(y) == Color::Red {
                    (*zp).color = Color::Black;
                    (*y).color = Color::Black;
                    (*zpp).color = Color::Red;
                    z = zpp;
                } else {
                    if z == (*zp).left {
                        z = zp;
                        self.rotate_right(z);
                    }
                    let zp = (*z).parent;
                    let zpp = (*zp).parent;
                    (*zp).color = Color::Black;
                    (*zpp).color = Color::Red;
                    self.rotate_left(zpp);
                }
            }
        }
        let root = self.root.load(Ordering::Relaxed);
        (*root).color = Color::Black;
    }

    unsafe fn rotate_left(&self, x: *mut Node) {
        let y = (*x).right;
        (*x).right = (*y).left;
        if !(*y).left.is_null() {
            (*(*y).left).parent = x;
        }
        (*y).parent = (*x).parent;
        if (*x).parent.is_null() {
            self.root.store(y, Ordering::Relaxed);
        } else if x == (*(*x).parent).left {
            (*(*x).parent).left = y;
        } else {
            (*(*x).parent).right = y;
        }
        (*y).left = x;
        (*x).parent = y;
    }

    unsafe fn rotate_right(&self, x: *mut Node) {
        let y = (*x).left;
        (*x).left = (*y).right;
        if !(*y).right.is_null() {
            (*(*y).right).parent = x;
        }
        (*y).parent = (*x).parent;
        if (*x).parent.is_null() {
            self.root.store(y, Ordering::Relaxed);
        } else if x == (*(*x).parent).right {
            (*(*x).parent).right = y;
        } else {
            (*(*x).parent).left = y;
        }
        (*y).right = x;
        (*x).parent = y;
    }

    unsafe fn transplant(&self, u: *mut Node, v: *mut Node) {
        let up = (*u).parent;
        if up.is_null() {
            self.root.store(v, Ordering::Relaxed);
        } else if u == (*up).left {
            (*up).left = v;
        } else {
            (*up).right = v;
        }
        if !v.is_null() {
            (*v).parent = up;
        }
    }

    unsafe fn minimum(&self, mut x: *mut Node) -> *mut Node {
        unsafe {
            while !(*x).left.is_null() {
                x = (*x).left;
            }
            x
        }
    }

    unsafe fn delete_node(&self, z: *mut Node) {
        let mut y = z;
        let mut y_original_color = (*y).color;
        let x: *mut Node;
        let x_parent: *mut Node;

        if (*z).left.is_null() {
            x = (*z).right;
            x_parent = (*z).parent;
            self.transplant(z, (*z).right);
        } else if (*z).right.is_null() {
            x = (*z).left;
            x_parent = (*z).parent;
            self.transplant(z, (*z).left);
        } else {
            y = self.minimum((*z).right);
            y_original_color = (*y).color;
            x = (*y).right;
            if (*y).parent == z {
                x_parent = y;
            } else {
                x_parent = (*y).parent;
                self.transplant(y, (*y).right);
                (*y).right = (*z).right;
                (*(*y).right).parent = y;
            }
            self.transplant(z, y);
            (*y).left = (*z).left;
            (*(*y).left).parent = y;
            (*y).color = (*z).color;
        }

        if y_original_color == Color::Black {
            self.delete_fixup(x, x_parent);
        }

        self.free_node(z);
    }

    unsafe fn delete_fixup(&self, mut x: *mut Node, mut x_parent: *mut Node) {
        while x != self.root.load(Ordering::Relaxed) && color_of(x) == Color::Black {
            if x_parent.is_null() {
                break;
            }
            if x == (*x_parent).left {
                let mut w = (*x_parent).right;
                if color_of(w) == Color::Red {
                    (*w).color = Color::Black;
                    (*x_parent).color = Color::Red;
                    self.rotate_left(x_parent);
                    w = (*x_parent).right;
                }
                if color_of((*w).left) == Color::Black && color_of((*w).right) == Color::Black {
                    (*w).color = Color::Red;
                    x = x_parent;
                    x_parent = (*x).parent;
                } else {
                    if color_of((*w).right) == Color::Black {
                        if !(*w).left.is_null() {
                            (*(*w).left).color = Color::Black;
                        }
                        (*w).color = Color::Red;
                        self.rotate_right(w);
                        w = (*x_parent).right;
                    }
                    (*w).color = (*x_parent).color;
                    (*x_parent).color = Color::Black;
                    if !(*w).right.is_null() {
                        (*(*w).right).color = Color::Black;
                    }
                    self.rotate_left(x_parent);
                    x = self.root.load(Ordering::Relaxed);
                    x_parent = null_mut();
                }
            } else {
                let mut w = (*x_parent).left;
                if color_of(w) == Color::Red {
                    (*w).color = Color::Black;
                    (*x_parent).color = Color::Red;
                    self.rotate_right(x_parent);
                    w = (*x_parent).left;
                }
                if color_of((*w).right) == Color::Black && color_of((*w).left) == Color::Black {
                    (*w).color = Color::Red;
                    x = x_parent;
                    x_parent = (*x).parent;
                } else {
                    if color_of((*w).left) == Color::Black {
                        if !(*w).right.is_null() {
                            (*(*w).right).color = Color::Black;
                        }
                        (*w).color = Color::Red;
                        self.rotate_left(w);
                        w = (*x_parent).left;
                    }
                    (*w).color = (*x_parent).color;
                    (*x_parent).color = Color::Black;
                    if !(*w).left.is_null() {
                        (*(*w).left).color = Color::Black;
                    }
                    self.rotate_right(x_parent);
                    x = self.root.load(Ordering::Relaxed);
                    x_parent = null_mut();
                }
            }
        }
        if !x.is_null() {
            (*x).color = Color::Black;
        }
    }

    unsafe fn alloc_chunk(&self) {
        let size = NODE_CHUNK * size_of::<Node>();
        record_mmap_call(size);
        let ptr = mmap_anonymous(
            null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE | MapFlags::NORESERVE,
        )
        .unwrap_or_else(|_| {
            RSMallocError::OutOfMemory.log_and_abort(
                null_mut(),
                "Cannot allocate BigAllocMap node chunk",
                None,
            )
        }) as *mut Node;

        for i in 0..NODE_CHUNK {
            let node = ptr.add(i);
            (*node).left = if i + 1 < NODE_CHUNK {
                ptr.add(i + 1)
            } else {
                null_mut()
            };
        }

        let old_head = self.free_list.load(Ordering::Relaxed);
        (*ptr.add(NODE_CHUNK - 1)).left = old_head;
        self.free_list.store(ptr, Ordering::Relaxed);
    }

    unsafe fn alloc_node(&self, key: usize, meta: BigAllocMeta) -> *mut Node {
        if self.free_list.load(Ordering::Relaxed).is_null() {
            self.alloc_chunk();
        }

        let head = self.free_list.load(Ordering::Relaxed);
        let next = (*head).left;
        self.free_list.store(next, Ordering::Relaxed);

        (*head).key = key;
        (*head).meta = meta;
        (*head).left = null_mut();
        (*head).right = null_mut();
        (*head).parent = null_mut();
        (*head).color = Color::Red;
        head
    }

    unsafe fn free_node(&self, node: *mut Node) {
        let head = self.free_list.load(Ordering::Relaxed);
        (*node).left = head;
        self.free_list.store(node, Ordering::Relaxed);
    }
}

unsafe impl Sync for RBTree {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    fn meta(size: usize) -> BigAllocMeta {
        BigAllocMeta {
            next: null_mut(),
            size,
            order: 0,
            buddy_region: 0,
            aligned: false,
        }
    }

    unsafe fn black_height(map: &RBTree, node: *mut Node) -> Result<usize, &'static str> {
        if node.is_null() {
            return Ok(1);
        }
        if color_of(node) == Color::Red {
            if color_of((*node).left) == Color::Red || color_of((*node).right) == Color::Red {
                return Err("red node has red child");
            }
        }
        let left = black_height(map, (*node).left)?;
        let right = black_height(map, (*node).right)?;
        if left != right {
            return Err("black height mismatch");
        }
        let inc = if color_of(node) == Color::Black { 1 } else { 0 };
        Ok(left + inc)
    }

    unsafe fn assert_valid_rb_tree(map: &RBTree) {
        let root = map.root.load(Ordering::Relaxed);
        assert_eq!(color_of(root), Color::Black);
        black_height(map, root).unwrap();
    }

    #[test]
    fn insert_get_replace_remove_matches_reference_hashmap() {
        let map = RBTree::new();
        let mut reference: StdHashMap<usize, usize> = StdHashMap::new();

        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next_rand = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        unsafe {
            for i in 0..5000usize {
                let op = next_rand() % 4;
                let key = ((next_rand() % 500) as usize) * 16 + 0x1000;
                match op {
                    0 | 1 => {
                        map.insert(key, meta(i));
                        reference.insert(key, i);
                    }
                    2 => {
                        let got = map.get(key).map(|m| m.size);
                        assert_eq!(got, reference.get(&key).copied());
                    }
                    _ => {
                        let got = map.remove(key).map(|m| m.size);
                        assert_eq!(got, reference.remove(&key));
                    }
                }
                assert_valid_rb_tree(&map);
            }

            for (key, size) in &reference {
                assert_eq!(map.get(*key).map(|m| m.size), Some(*size));
            }
        }
    }

    #[test]
    fn replace_returns_old_value_and_leaves_key_present() {
        let map = RBTree::new();
        unsafe {
            assert!(map.replace(42, meta(1)).is_none());
            map.insert(42, meta(1));
            let old = map.replace(42, meta(2));
            assert_eq!(old.map(|m| m.size), Some(1));
            assert_eq!(map.get(42).map(|m| m.size), Some(2));
            assert_valid_rb_tree(&map);
        }
    }
}
