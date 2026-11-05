use crate::arch::x86_64::{guest_context::GuestContext, hypercall::PONG_MAGIC};
#[cfg(any(feature = "svm-npt", feature = "buffer-log"))]
use crate::arch::x86_64::svm::state::SvmVcpu;

#[cfg(any(feature = "buffer-log", feature = "svm-npt"))]
use crate::arch::x86_64::guest_memory;

pub(super) fn ping(ctx: &mut GuestContext) {
    ctx.rax = PONG_MAGIC ^ ctx.rdx;
}

#[cfg(feature = "svm-npt")]
pub(super) fn hide_self(svm: &mut SvmVcpu, ctx: &mut GuestContext) {
    ctx.rax = match crate::arch::x86_64::svm::hide_self::hide(svm) {
        Ok(hidden) => hidden as u64,
        Err(error) => error.as_u32() as u64,
    };
}

#[cfg(feature = "svm-npt")]
pub(super) fn read_self(svm: &mut SvmVcpu, ctx: &mut GuestContext, through_ept: bool) {
    use crate::arch::x86_64::{memory::PAGE_SIZE, svm::hide_self};

    let index = ctx.rdx as u32;
    let address = ctx.r8;
    let mut page = [0u8; PAGE_SIZE];

    let ok = if through_ept {
        hide_self::read_ept(svm, index, &mut page)
    } else {
        hide_self::read_root(index, &mut page)
    };

    ctx.rax = if ok && write_guest_buffer(svm, address, &page) {
        PAGE_SIZE as u64
    } else {
        0
    };
}

#[cfg(feature = "buffer-log")]
pub(super) fn logger_info(ctx: &mut GuestContext) {
    let stats = crate::log::stats();

    ctx.rax = stats.total;
    ctx.rdx = stats.dropped;
    ctx.r8 = stats.buffered as u64;
}

#[cfg(feature = "buffer-log")]
pub(super) fn flush_logs(svm: &SvmVcpu, ctx: &mut GuestContext) {
    ctx.rax = flush(svm, ctx.rdx, ctx.r8);
}

#[cfg(feature = "buffer-log")]
fn flush(svm: &SvmVcpu, destination: u64, capacity: u64) -> u64 {
    use crate::log;

    let capacity = capacity.min(log::MAX_MSG_COUNT as u64) as u32;

    if capacity == 0 {
        return 0;
    }

    let cr3 = svm.vmcb.save.cr3;
    let user = svm.vmcb.save.cs.selector & 3 == 3;
    let write_protect = svm.vmcb.save.cr0 & (1 << 16) != 0;
    let mut offset = 0u64;

    let copied = log::flush(capacity, |message| {
        let Some(address) = destination.checked_add(offset) else {
            return false;
        };

        match guest_memory::copy_to_guest(cr3, address, message.as_bytes(), user, write_protect) {
            Ok(()) => {
                offset += log::LOGGER_MSG_SIZE as u64;
                true
            }
            Err(_) => false,
        }
    });

    copied.unwrap_or(0) as u64
}

#[cfg(feature = "svm-npt")]
fn write_guest_buffer(svm: &SvmVcpu, address: u64, source: &[u8]) -> bool {
    let cr3 = svm.vmcb.save.cr3;
    let user = svm.vmcb.save.cs.selector & 3 == 3;
    let write_protect = svm.vmcb.save.cr0 & (1 << 16) != 0;

    guest_memory::copy_to_guest(cr3, address, source, user, write_protect).is_ok()
}
