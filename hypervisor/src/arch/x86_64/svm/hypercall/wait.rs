use crate::arch::x86_64::{
    guest_context::GuestContext, hypercall::Hypercall, svm::{hook, state::SvmVcpu},
};

pub(super) fn dispatch(svm: &SvmVcpu, ctx: &mut GuestContext, command: Hypercall) {
    match command {
        Hypercall::UserBreakpointWaitList => ctx.rax = hook::wait_list(svm, ctx.rdx, ctx.r8),
        Hypercall::UserBreakpointWaitReply => {
            ctx.rax = hook::wait_reply(ctx.rdx, ctx.r8 as u32, ctx.r9) as u64;
        }
        Hypercall::UserBreakpointWaitTimeout => ctx.rax = hook::wait_timeout(ctx.rdx),
        Hypercall::UserBreakpointWaitSetReg => {
            ctx.rax = hook::wait_set_reg(ctx.rdx, ctx.r8 as u8, ctx.r9) as u64;
        }
        Hypercall::UserBreakpointWaitFilter => {
            ctx.rax = hook::wait_filter(ctx.rdx, ctx.r8) as u64;
        }
        Hypercall::UserBreakpointWaitCancel => ctx.rax = hook::wait_cancel(ctx.rdx),
        _ => ctx.rax = 3,
    }
}
