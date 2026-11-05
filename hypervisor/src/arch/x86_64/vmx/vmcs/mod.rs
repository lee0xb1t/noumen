// The VMCS is a complete hardware ABI surface. Keeping the full field accessor
// set makes the layout auditable; not every field is used by this hypervisor.
#![allow(dead_code)]

pub mod control;
pub mod guest;
pub mod host;
pub mod types;
