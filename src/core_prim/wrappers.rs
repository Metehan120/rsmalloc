#![allow(dead_code)]
use std::{
    ops::{Deref, DerefMut, Index},
    ptr::null_mut,
};

macro_rules! impl_into {
    ($type:ident, $($u:ty),*) => {
        $(
            impl<T> Into<$u> for $type<T> {
                fn into(self) -> $u {
                    self.0 as $u
                }
            }
        )*
    };
}

macro_rules! impl_from {
    ($type:ident, $($u:ty),*) => {
        $(
            impl<T> From<$u> for $type<T> {
                fn from(ptr: $u) -> Self {
                    Self(ptr as *mut T)
                }
            }
        )*
    };
}

macro_rules! impl_conversions {
    ($type:ident) => {
        impl_into!($type, usize, u64);
        impl_from!($type, usize, u64);

        impl<T> From<*mut T> for $type<T> {
            fn from(ptr: *mut T) -> Self {
                Self(ptr)
            }
        }

        impl<T> From<*const T> for $type<T> {
            fn from(ptr: *const T) -> Self {
                Self(ptr as *mut T)
            }
        }
    };
}

#[must_use]
#[repr(transparent)]
pub struct Owned<T>(*mut T);

unsafe impl<T> Sync for Owned<T> {}
unsafe impl<T> Send for Owned<T> {}

impl<T> Owned<T> {
    pub const NULL: Self = Self(null_mut());

    #[inline(always)]
    pub const fn get_actual_header(&self) -> Self {
        Self(unsafe { self.0.sub(1) })
    }

    pub const fn get_offset(&self, offset: usize) -> Self {
        Self(unsafe { self.0.add(offset) })
    }

    #[inline(always)]
    pub const fn walk_header(&self) -> Self {
        Self(unsafe { self.0.add(1) })
    }

    #[inline(always)]
    pub const fn cast<Y>(&self) -> Owned<Y> {
        Owned(self.0 as _)
    }

    #[inline(always)]
    pub const fn cast_as_ptr<Y>(&self) -> *mut Y {
        self.0 as *mut Y
    }

    #[inline(always)]
    pub const fn as_ptr(&self) -> *mut T {
        self.0 as *mut T
    }

    #[inline(always)]
    pub const fn apply_unsafe(&self) -> MaybeNull<T> {
        MaybeNull(self.0)
    }

    #[inline(always)]
    pub fn cast_usize(&self) -> usize {
        self.0 as usize
    }
}

impl<T> Deref for Owned<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        unsafe { &*self.0 }
    }
}

impl<T> DerefMut for Owned<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0 }
    }
}

impl<T> Index<usize> for Owned<T> {
    type Output = T;
    #[inline(always)]
    fn index(&self, index: usize) -> &Self::Output {
        unsafe { &*self.0.add(index) }
    }
}

impl_conversions!(Owned);

#[must_use]
#[repr(transparent)]
pub struct MaybeNull<T>(*mut T);

unsafe impl<T> Sync for MaybeNull<T> {}
unsafe impl<T> Send for MaybeNull<T> {}

impl<T> MaybeNull<T> {
    #[inline(always)]
    pub const fn new(ptr: *mut T) -> Self {
        Self(ptr as *mut T)
    }

    #[inline(always)]
    pub fn cast_usize(&self) -> usize {
        self.0 as usize
    }

    #[inline(always)]
    pub const fn is_null(&self) -> bool {
        self.0.is_null()
    }

    #[inline(always)]
    pub const fn get_actual_header(&self) -> Self {
        Self(unsafe { self.0.sub(1) })
    }

    #[inline(always)]
    pub const fn walk_header(&self) -> Self {
        Self(unsafe { self.0.add(1) })
    }

    #[inline(always)]
    pub const fn cast<Y>(&self) -> MaybeNull<Y> {
        MaybeNull(self.0 as _)
    }

    #[inline(always)]
    pub const fn cast_as_ptr<Y>(&self) -> *mut Y {
        self.0 as *mut Y
    }

    #[inline(always)]
    pub const fn as_ptr(&self) -> *mut T {
        self.0 as *mut T
    }

    #[inline(always)]
    pub const fn apply_safe(&self) -> Owned<T> {
        Owned(self.0)
    }

    pub const NULL: Self = Self(null_mut());
}

impl_conversions!(MaybeNull);
