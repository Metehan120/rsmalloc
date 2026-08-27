use crate::{
    Header, RseqResult, core_prim::wrappers::UnsafePointer, rseq_core::rseq_offsets::rseq,
};

pub trait GenericCache {
    unsafe fn push(&self, class: usize, header: *mut Header);
    unsafe fn pop(&self, class: usize) -> UnsafePointer<Header>;
    unsafe fn push_tailed(
        &self,
        class: usize,
        header: *mut Header,
        tail: *mut Header,
        batch_size: usize,
    );
}

pub trait RseqCoreTrait {
    unsafe fn push(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        usage_ptr: *mut usize,
    ) -> RseqResult;
    unsafe fn push_tailed(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        tail: *mut Header,
        usage_ptr: *mut usize,
        batch_total: usize,
    ) -> RseqResult;
    unsafe fn pop(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        usage_ptr: *mut usize,
    ) -> RseqResult;
}

pub trait Lock {
    type Out;

    type Guard<'a, T>
    where
        Self: 'a,
        T: 'a;

    type LockState;
    type LockError<G>;

    fn lock(&self) -> Self::Guard<'_, Self::Out>;
    fn try_lock(&self) -> Self::LockError<Self::Guard<'_, Self::Out>>;
    fn spin_until_unlock(&self);
    #[allow(dead_code)]
    fn get_lock(&self) -> Self::LockState;
    fn unlock(&self);
}

pub mod global_alloc {
    pub trait RawInterface {
        type TrimIn;
        type TrimOut;

        unsafe fn rs_alloc(&self, size: usize) -> *mut u8;
        unsafe fn rs_free(&self, ptr: *mut u8);
        unsafe fn rs_realloc(&self, old: *mut u8, new_size: usize) -> *mut u8;
        unsafe fn rs_aligned(&self, alignment: usize, size: usize) -> *mut u8;
        unsafe fn rs_zeroed(&self, size: usize) -> *mut u8;
        unsafe fn trim(&self, trim_size: Self::TrimIn) -> Self::TrimOut;
    }
}
