use crate::arch::x86_64::{
    guest_context::GuestContext,
    resume::EmulationResult,
    vcpu::Vcpu,
};

/// 当前 guest CR3（VM-exit 时刻）；低 12 位的 PCID 由 `mem::virt` 掩掉。
#[cfg(all(feature = "mem", feature = "vmx-ept"))]
#[inline]
fn current_cr3() -> EmulationResult<u64> {
    Ok(crate::arch::x86_64::vmx::vmcs::guest::Cr3::read()?.as_raw())
}

#[cfg(all(feature = "mem", feature = "vmx-ept"))]
pub(super) fn query(context: &mut GuestContext) -> EmulationResult<()> {
    use crate::arch::x86_64::vmx::ept::mem;

    if !mem::available() {
        context.rax = mem::MemError::Unsupported.as_u64();
        return Ok(());
    }

    context.rax = mem::ABI_VERSION as u64;
    context.rdx = mem::MAX_BATCH_OPS as u64;
    context.r8 = mem::max_batch_bytes() as u64;
    context.r9 = crate::arch::x86_64::paging::DIRECT_MAP_SIZE;

    Ok(())
}

#[cfg(all(feature = "mem", feature = "vmx-ept"))]
pub(super) fn batch(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()> {
    use crate::arch::x86_64::vmx::ept::mem;

    if !mem::available() {
        context.rax = mem::MemError::Unsupported.as_u64();
        return Ok(());
    }

    let cr3 = current_cr3()?;
    let dry_run = context.r9 & 1 != 0;
    let processed = mem::ring::execute(
        cr3,
        context.rdx,
        context.r8,
        dry_run,
        vcpu.cpu_id as usize,
    );

    context.rax = processed as u64;

    Ok(())
}

#[cfg(all(feature = "mem", feature = "vmx-ept"))]
pub(super) fn scan(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()> {
    use crate::arch::x86_64::vmx::ept::mem;

    if !mem::available() {
        context.rax = mem::MemError::Unsupported.as_u64();
        return Ok(());
    }

    let need = core::mem::size_of::<mem::MemScanRequest>() as u64;

    if context.rdx == 0 || context.r8 < need {
        context.rax = mem::MemError::BadEntry.as_u64();
        return Ok(());
    }

    let cr3 = current_cr3()?;

    let mut request = match mem::virt::read_struct::<mem::MemScanRequest>(cr3, context.rdx) {
        Ok(request) => request,
        Err(error) => {
            context.rax = error.as_u64();
            return Ok(());
        }
    };

    mem::scan(&mut request, cr3);

    if let Err(error) = mem::virt::write_struct(cr3, context.rdx, &request) {
        context.rax = error.as_u64();
        return Ok(());
    }

    mem::stats::record_scan(vcpu.cpu_id as usize, request.scanned as u64, request.tsc);

    context.rax = request.status as u64;

    Ok(())
}

#[cfg(all(feature = "mem", feature = "vmx-ept"))]
pub(super) fn map(context: &mut GuestContext) -> EmulationResult<()> {
    use crate::arch::x86_64::vmx::ept::mem;

    if !mem::available() {
        context.rax = mem::MemError::Unsupported.as_u64();
        return Ok(());
    }

    let cr3 = current_cr3()?;
    let capacity = context.r8 as usize;
    let total = mem::phys::range_total();
    let mut written = 0usize;

    while written < capacity {
        let Some(range) = mem::phys::range_at(written) else {
            break;
        };
        let Some(va) = context.rdx.checked_add((written * 16) as u64) else {
            break;
        };

        let mut raw = [0u8; 16];
        raw[0..8].copy_from_slice(&range.start.to_le_bytes());
        raw[8..16].copy_from_slice(&range.size.to_le_bytes());

        if mem::virt::write(cr3, va, &raw).is_err() {
            break;
        }

        written += 1;
    }

    context.rax = written as u64;
    context.rdx = total as u64;

    Ok(())
}

#[cfg(all(feature = "mem", feature = "vmx-ept"))]
pub(super) fn set_phys_range(context: &mut GuestContext) -> EmulationResult<()> {
    use crate::arch::x86_64::vmx::ept::mem;

    if !mem::available() {
        context.rax = mem::MemError::Unsupported.as_u64();
        return Ok(());
    }

    context.rax = mem::phys::set_ranges(
        crate::hv::system_cr3(),
        context.rdx,
        context.r8 as u32,
    ) as u64;

    Ok(())
}

#[cfg(all(feature = "mem", feature = "vmx-ept"))]
pub(super) fn stats(context: &mut GuestContext) -> EmulationResult<()> {
    use crate::arch::x86_64::vmx::ept::mem;

    if !mem::available() {
        context.rax = mem::MemError::Unsupported.as_u64();
        return Ok(());
    }

    let cr3 = current_cr3()?;
    let capacity = context.r8 as usize;
    let cpu_count = crate::hv::GLOBAL_ARGS.get().num_cpus as usize;
    let total = cpu_count.min(mem::stats::capacity());
    let mut written = 0usize;

    while written < capacity && written < total {
        let Some(stats) = mem::stats::snapshot(written) else {
            break;
        };
        let Some(va) = context.rdx.checked_add((written * 48) as u64) else {
            break;
        };

        if mem::virt::write_struct(cr3, va, &stats).is_err() {
            break;
        }

        written += 1;
    }

    context.rax = written as u64;
    context.rdx = total as u64;

    Ok(())
}
