//! `RDTSC` / `RDTSCP` fallback emulation.
//!
//! 当前不做 TSC 虚拟化：`RDTSC exiting` 正常为 0，只有 CPU 或外层 VMM 强制该
//! 控制位时才会走到这里；返回物理 TSC 并跳过指令。

use crate::arch::x86_64::{guest_context::GuestContext, resume::EmulationResult, vcpu::Vcpu};

use super::skip_instruction;

pub(super) fn emulate_rdtsc(_vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let tsc = unsafe { x86::time::rdtsc() };

    context.rax = tsc & 0xffff_ffff;
    context.rdx = (tsc >> 32) & 0xffff_ffff;

    skip_instruction()
}

pub(super) fn emulate_rdtscp(_vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let (tsc, aux) = unsafe { x86::time::rdtscp() };

    context.rax = tsc & 0xffff_ffff;
    context.rdx = (tsc >> 32) & 0xffff_ffff;
    context.rcx = aux as u64;

    skip_instruction()
}
