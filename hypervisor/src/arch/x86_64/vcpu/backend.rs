use crate::{
    arch::x86_64::{host_state::HostState, vcpu::Vcpu},
    error::HvResult,
};

pub trait VirtualizationBackend {
    fn setup(&mut self, host: &HostState, vcpu: *const Vcpu) -> HvResult<()>;
    fn launch(&self) -> HvResult<()>;
}
