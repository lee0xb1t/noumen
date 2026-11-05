use crate::arch::x86_64::{
    guest_context::GuestContext,
    svm::{hook as npt_hook, state::SvmVcpu},
};

pub(super) fn install(svm: &mut SvmVcpu, ctx: &mut GuestContext) {
    ctx.rax = npt_hook::install(svm, ctx.rdx, ctx.r8 as u32, ctx.r9) as u64;
}

pub(super) fn install_method(svm: &mut SvmVcpu, ctx: &mut GuestContext) {
    let encoded = ctx.r9;
    let kind = (encoded & 0xff) as u32;
    let page_hint = (encoded >> 8) << 12;

    ctx.rax = npt_hook::install_method(svm, ctx.rdx, kind, page_hint, ctx.r8 as u32) as u64;
}

pub(super) fn install_npt(svm: &mut SvmVcpu, ctx: &mut GuestContext) {
    ctx.rax = npt_hook::install_npt(svm, ctx.rdx, ctx.r8, ctx.r9, ctx.r10) as u64;
}

pub(super) fn remove(svm: &mut SvmVcpu, ctx: &mut GuestContext) {
    ctx.rax = npt_hook::remove(svm, ctx.rdx) as u64;
}

pub(super) fn remove_page(svm: &mut SvmVcpu, ctx: &mut GuestContext) {
    ctx.rax = npt_hook::remove_page(svm, ctx.rdx) as u64;
}

pub(super) fn oneshot(ctx: &mut GuestContext) {
    ctx.rax = npt_hook::set_oneshot(ctx.rdx, ctx.r8 != 0) as u64;
}

pub(super) fn commit(ctx: &mut GuestContext) {
    ctx.rax = npt_hook::commit(ctx.rdx) as u64;
}
