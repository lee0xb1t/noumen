//! Nested paging (NPT).

pub mod data;
pub mod hide_self;
pub mod hook;
pub mod mem;
pub mod state;
pub mod tlb;
pub mod types;

// Host tests compile these files as top-level modules; re-export `vmcb` so
// `super::vmcb` resolves in both layouts.
pub use super::vmcb;

pub use data::NptData;
pub use state::NptView;
pub use types::NptEntry;
