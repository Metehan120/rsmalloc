use std::{
    ops::{Deref, DerefMut},
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

        impl<T> Into<*mut T> for $type<T> {
            fn into(self) -> *mut T {
                self.0
            }
        }
    };
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
#[must_use]
pub struct SafePointer<T>(*mut T);

unsafe impl<T> Sync for SafePointer<T> {}
unsafe impl<T> Send for SafePointer<T> {}

impl<T> SafePointer<T> {
    #[inline(always)]
    pub const fn get_actual_header(&self) -> Self {
        Self(unsafe { self.0.sub(1) })
    }

    #[inline(always)]
    pub const fn walk_header(&self) -> Self {
        Self(unsafe { self.0.add(1) })
    }

    /*#[inline(always)]
       pub const fn cast<Y>(&self) -> SafePointer<Y> {
           SafePointer(self.0 as _)
       }
    */

    #[inline(always)]
    pub const fn cast_as_ptr<Y>(&self) -> *mut Y {
        self.0 as *mut Y
    }

    #[inline(always)]
    pub const fn as_ptr(&self) -> *mut T {
        self.0 as *mut T
    }

    #[inline(always)]
    pub const fn apply_unsafe(&self) -> UnsafePointer<T> {
        UnsafePointer(self.0)
    }

    #[inline(always)]
    pub fn cast_usize(&self) -> usize {
        self.0 as usize
    }
}

impl<T> Deref for SafePointer<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.0 }
    }
}

impl<T> DerefMut for SafePointer<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0 }
    }
}

impl_conversions!(SafePointer);

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
#[must_use]
pub struct UnsafePointer<T>(*mut T);

unsafe impl<T> Sync for UnsafePointer<T> {}
unsafe impl<T> Send for UnsafePointer<T> {}

impl<T> UnsafePointer<T> {
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
    pub const fn cast<Y>(&self) -> UnsafePointer<Y> {
        UnsafePointer(self.0 as _)
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
    pub const fn apply_safe(&self) -> SafePointer<T> {
        SafePointer(self.0)
    }

    pub const NULL: Self = Self(null_mut());
}

impl_conversions!(UnsafePointer);
