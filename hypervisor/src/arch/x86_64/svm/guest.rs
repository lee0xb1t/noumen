//! Guest state initialisation for the first `VMRUN`.

use crate::arch::x86_64::{
    cr, safe_routine, segment,
    svm::vmcb::{
        Vmcb,
        save::{StateSaveArea, VmcbSeg, attrib_from_access_rights},
    },
};

const MSR_EFER: u32 = 0xc000_0080;
const MSR_STAR: u32 = 0xc000_0081;
const MSR_LSTAR: u32 = 0xc000_0082;
const MSR_CSTAR: u32 = 0xc000_0083;
const MSR_SFMASK: u32 = 0xc000_0084;
const MSR_FS_BASE: u32 = 0xc000_0100;
const MSR_GS_BASE: u32 = 0xc000_0101;
const MSR_KERNEL_GS_BASE: u32 = 0xc000_0102;
const MSR_SYSENTER_CS: u32 = 0x174;
const MSR_SYSENTER_ESP: u32 = 0x175;
const MSR_SYSENTER_EIP: u32 = 0x176;
/// `IA32_DEBUGCTL`. **Not** 0x1A0: that is Intel's `IA32_MISC_ENABLE`, which
/// does not exist on AMD and raises `#GP` when read.
const MSR_DEBUGCTL: u32 = 0x1d9;
const MSR_PAT: u32 = 0x277;

const EFER_SVME: u64 = 1 << 12;

unsafe fn rdmsr(msr: u32) -> u64 {
    unsafe { x86::msr::rdmsr(msr) }
}

fn seg_from_gdt(gdtr: &segment::Gdtr, selector: &segment::SegmentSelector) -> VmcbSeg {
    let access = segment::segment_access(gdtr, selector);

    VmcbSeg {
        selector: selector.raw(),
        attrib: attrib_from_access_rights(access.raw()),
        limit: segment_limit(selector),
        base: segment::segment_base(gdtr, selector),
    }
}

/// `LSL` faults on a null selector; a null segment is unusable and has limit 0.
fn segment_limit(selector: &segment::SegmentSelector) -> u32 {
    if selector.is_null() {
        0
    } else {
        segment::segment_limit(selector)
    }
}

/// Descriptor-table register: the selector/attrib fields are unused by hardware.
fn table_from_gdtr(limit: u16, base: u64) -> VmcbSeg {
    VmcbSeg {
        selector: 0,
        attrib: 0,
        limit: limit as u32,
        base,
    }
}

/// Copy the current CPU state into `vmcb.save`.
///
/// Returns the guest FS base. The caller must then point the *host* FS base at
/// the `Vcpu` ( [`crate::arch::x86_64::vcpu::Vcpu::current`] depends on it); the
/// guest value stays in the VMCB and is loaded by `VMRUN`.
pub fn write_guest_state(vmcb: &mut Vmcb) -> u64 {
    let gdtr = segment::Gdtr::sgdt();
    let idtr = segment::Idtr::sidt();

    let cs = segment::SegmentSelector::read_cs();
    let ss = segment::SegmentSelector::read_ss();
    let ds = segment::SegmentSelector::read_ds();
    let es = segment::SegmentSelector::read_es();
    let fs = segment::SegmentSelector::read_fs();
    let gs = segment::SegmentSelector::read_gs();
    let tr = segment::SegmentSelector::read_tr();
    let ldtr = segment::SegmentSelector::read_ldtr();

    let save: &mut StateSaveArea = &mut vmcb.save;

    save.es = seg_from_gdt(&gdtr, &es);
    save.cs = seg_from_gdt(&gdtr, &cs);
    save.ss = seg_from_gdt(&gdtr, &ss);
    save.ds = seg_from_gdt(&gdtr, &ds);
    save.fs = seg_from_gdt(&gdtr, &fs);
    save.gs = seg_from_gdt(&gdtr, &gs);
    save.tr = seg_from_gdt(&gdtr, &tr);
    save.ldtr = seg_from_gdt(&gdtr, &ldtr);

    save.gdtr = table_from_gdtr(gdtr.limit, gdtr.base_address);
    save.idtr = table_from_gdtr(idtr.limit, idtr.base_address);

    // Hidden FS/GS bases come from the MSRs, not from the descriptor base.
    let fs_base = unsafe { rdmsr(MSR_FS_BASE) };
    save.fs.base = fs_base;
    save.gs.base = unsafe { rdmsr(MSR_GS_BASE) };

    save.cpl = cs.rpl();
    save.vmpl = 0;

    // The guest runs with SVME set: VMRUN already checked the host's SVME, and
    // clearing it in guest state is undefined. RDMSR hides it from the guest.
    save.efer = unsafe { rdmsr(MSR_EFER) } | EFER_SVME;
    save.cr0 = cr::Cr0::read().bits();
    save.cr3 = unsafe { x86::controlregs::cr3() };
    save.cr4 = cr::Cr4::read().bits();
    save.cr2 = unsafe { x86::controlregs::cr2() } as u64;
    save.dr7 = unsafe { x86::debugregs::dr7() }.0 as u64;
    save.dr6 = unsafe { x86::debugregs::dr6() }.bits() as u64;
    save.rflags = x86::bits64::rflags::read().bits();

    save.star = unsafe { rdmsr(MSR_STAR) };
    save.lstar = unsafe { rdmsr(MSR_LSTAR) };
    save.cstar = unsafe { rdmsr(MSR_CSTAR) };
    save.sfmask = unsafe { rdmsr(MSR_SFMASK) };
    save.kernel_gs_base = unsafe { rdmsr(MSR_KERNEL_GS_BASE) };
    save.sysenter_cs = unsafe { rdmsr(MSR_SYSENTER_CS) };
    save.sysenter_esp = unsafe { rdmsr(MSR_SYSENTER_ESP) };
    save.sysenter_eip = unsafe { rdmsr(MSR_SYSENTER_EIP) };
    save.g_pat = unsafe { rdmsr(MSR_PAT) };

    // Optional: some CPUs may not implement DEBUGCTL. A guest value of 0 just
    // disables LBR/BTS, which is the safe fallback.
    let mut debug_info = safe_routine::HostExceptionInfo::new();
    save.dbgctl = safe_routine::rdmsr_safe(&mut debug_info, MSR_DEBUGCTL).unwrap_or(0);

    // RIP/RSP/RAX are written by the launch trampoline (RIP/RSP) or start at 0.
    save.rip = 0;
    save.rsp = 0;
    save.rax = 0;

    fs_base
}

/// Point the host FS base at `vcpu` so [`crate::arch::x86_64::vcpu::Vcpu::current`]
/// keeps working after `vmload` restores the host state on every exit.
pub fn set_host_fs_base(vcpu: *const crate::arch::x86_64::vcpu::Vcpu) {
    unsafe { x86::msr::wrmsr(MSR_FS_BASE, vcpu as u64) };
}

/// Switch to the hypervisor page table before `VMRUN`.
///
/// `VMRUN` saves the host CR3 into HSAVE and restores it on every `#VMEXIT`, so
/// the root context keeps the direct map and the kernel mapping. The guest CR3
/// was captured into the VMCB and is loaded by `VMRUN`.
pub fn switch_to_host_cr3(cr3: u64) {
    // SAFETY: `cr3` is the PML4 physical address built by `hv::init_host_page`;
    // it maps the kernel image, the page pool and the direct map used by root.
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack));
    }
}
