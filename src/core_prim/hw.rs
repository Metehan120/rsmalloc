#[cfg(feature = "debug-exact")]
mod clock {
    pub struct CycleClock {
        aux: u32,
        start: u64,
    }

    impl CycleClock {
        pub fn new() -> Self {
            let mut aux = 0;

            #[cfg(target_arch = "x86_64")]
            let start = unsafe { std::arch::x86_64::__rdtscp(&mut aux) };

            CycleClock { aux, start }
        }

        pub fn elapsed(&mut self) -> u64 {
            #[cfg(target_arch = "x86_64")]
            let end = unsafe { std::arch::x86_64::__rdtscp(&mut self.aux) };

            end - self.start
        }
    }
}

pub struct HardwareFeature;

#[repr(transparent)]
pub struct SafeToPrefetch<T>(*mut T);

impl<T> SafeToPrefetch<T> {
    #[inline(always)]
    pub const fn new(ptr: *mut T) -> Self {
        Self(ptr)
    }

    #[inline(always)]
    const fn to_const(&self) -> *const i8 {
        self.0 as *const i8
    }
}

#[repr(u8)]
#[allow(dead_code)]
pub enum PrefetchHint {
    PreferL1,
    PreferL2,
    PreferL3,
}

impl HardwareFeature {
    #[inline(always)]
    pub fn prefetch<T>(&self, pointer: SafeToPrefetch<T>, hint: PrefetchHint) {
        #[cfg(target_arch = "x86_64")]
        {
            use std::arch::x86_64::{_MM_HINT_T0, _MM_HINT_T1, _MM_HINT_T2, _mm_prefetch};

            match hint {
                PrefetchHint::PreferL1 => unsafe { _mm_prefetch(pointer.to_const(), _MM_HINT_T0) },
                PrefetchHint::PreferL2 => unsafe { _mm_prefetch(pointer.to_const(), _MM_HINT_T1) },
                PrefetchHint::PreferL3 => unsafe { _mm_prefetch(pointer.to_const(), _MM_HINT_T2) },
            }
        }
    }

    #[cfg(feature = "debug-exact")]
    #[inline(always)]
    pub fn new_cycle_clock() -> clock::CycleClock {
        clock::CycleClock::new()
    }
}
