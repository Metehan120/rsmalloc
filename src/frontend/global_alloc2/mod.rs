pub mod alloc;
#[cfg(any(feature = "native-allocation-api", doc))]
pub mod allocation_api;
pub mod config;
mod debug;
pub mod helpers;
mod raw;

pub mod prelude {
    pub use crate::v2::alloc::RSMalloc;
    pub use crate::v2::alloc::RSMallocCoreAPI;
    pub use crate::v2::alloc::SimpleTrimSize;
    pub use crate::v2::config::*;
}
