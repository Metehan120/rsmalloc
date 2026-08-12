use crate::{Header, core_prim::wrappers::UnsafePointer, rseq_core::rseq_offsets::rseq};

pub(crate) trait GenericCache {
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

pub(crate) trait RseqCoreTrait {
    unsafe fn push(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        usage_ptr: *mut usize,
    ) -> usize;
    unsafe fn push_tailed(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        tail: *mut Header,
        usage_ptr: *mut usize,
        batch_total: usize,
    ) -> usize;
    unsafe fn pop(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        usage_ptr: *mut usize,
    ) -> *mut Header;
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
}
