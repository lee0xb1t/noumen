use crate::arch::x86_64::{
    guest_context::GuestContext,
    hypercall::Hypercall,
    svm::{mem, scan, state::SvmVcpu},
};

pub(super) fn dispatch(svm: &SvmVcpu, ctx: &mut GuestContext, command: Hypercall) {
    match command {
        Hypercall::MemQuery => {
            ctx.rax = mem::ABI_VERSION as u64;
            ctx.rdx = mem::MAX_BATCH_OPS as u64;
            ctx.r8 = mem::max_batch_bytes() as u64;
            ctx.r9 = crate::arch::x86_64::paging::DIRECT_MAP_SIZE;
        }
        Hypercall::MemBatch => {
            ctx.rax = mem::execute(
                svm.vmcb.save.cr3,
                ctx.rdx,
                ctx.r8,
                ctx.r9 & 1 != 0,
                svm.cpu_id as usize,
            ) as u64;
        }
        Hypercall::MemMap => {
            ctx.rax = mem::write_ranges(ctx.rdx, ctx.r8 as usize, svm.vmcb.save.cr3);
            ctx.rdx = mem::range_total();
        }
        Hypercall::MemStats => {
            ctx.rax = mem::write_stats(ctx.rdx, ctx.r8 as usize, svm.vmcb.save.cr3);
        }
        Hypercall::MemScan => {
            ctx.rax = scan::scan_guest(
                ctx.rdx,
                ctx.r8,
                svm.vmcb.save.cr3,
                svm.cpu_id as usize,
            );
        }
        Hypercall::SetPhysMemoryRange => {
            ctx.rax = mem::set_ranges(crate::hv::system_cr3(), ctx.rdx, ctx.r8 as u32) as u64;
        }
        _ => ctx.rax = 3,
    }
}
