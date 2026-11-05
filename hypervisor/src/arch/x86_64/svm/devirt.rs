//! Leaving virtualization on one CPU.

use core::arch::asm;

use crate::{
    arch::x86_64::{
        svm::{state::EFER_SVME, vmcb::save::StateSaveArea},
        vcpu::Vcpu,
    },
    klog,
};

/// Register frame consumed by `svm_leave_to_guest` in `stub.S`.
#[repr(C)]
pub struct DevirtFrame {
    pub cr0: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub efer: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rflags: u64,
    pub cs: u64,
    pub ss: u64,
}

const _: () = assert!(core::mem::size_of::<DevirtFrame>() == 9 * 8);
const _: () = assert!(core::mem::offset_of!(DevirtFrame, rip) == 32);
const _: () = assert!(core::mem::offset_of!(DevirtFrame, rsp) == 40);
const _: () = assert!(core::mem::offset_of!(DevirtFrame, rflags) == 48);
const _: () = assert!(core::mem::offset_of!(DevirtFrame, cs) == 56);
const _: () = assert!(core::mem::offset_of!(DevirtFrame, ss) == 64);

#[repr(C, packed)]
struct DescriptorTableRegister {
    limit: u16,
    base: u64,
}

/// Restore the guest and leave SVM operation on this CPU. Never returns.
pub fn devirtualize(vcpu: &mut Vcpu) -> ! {
    let svm = vcpu.backend.as_svm_mut();

    // A temporary single-step breakpoint must not survive into the guest.
    #[cfg(feature = "svm-npt")]
    {
        crate::arch::x86_64::svm::hook::restore_stop_after();
        crate::arch::x86_64::svm::hook::shutdown(svm);
    }

    let save: &StateSaveArea = &svm.vmcb.save;

    let frame = DevirtFrame {
        cr0: save.cr0,
        cr3: save.cr3,
        cr4: save.cr4,
        efer: save.efer & !EFER_SVME,
        rip: save.rip,
        rsp: save.rsp,
        rflags: save.rflags,
        cs: save.cs.selector as u64,
        ss: save.ss.selector as u64,
    };

    let gdt = DescriptorTableRegister {
        limit: save.gdtr.limit as u16,
        base: save.gdtr.base,
    };
    let idt = DescriptorTableRegister {
        limit: save.idtr.limit as u16,
        base: save.idtr.base,
    };

    let vmcb_pa = svm.vmcb.phys().as_u64();
    let cpu_id = vcpu.cpu_id;

    klog!(
        "SVM: stopping virtualization on cpu#{} (rip={:#x} rsp={:#x})",
        cpu_id,
        frame.rip,
        frame.rsp
    );

    // SAFETY: VMCB/HSAVE are live; the frame is a local with the layout asserted
    // above; `svm_leave_to_guest` never returns.
    unsafe {
        svm_vmload(vmcb_pa);
        load_gdt(&gdt);
        load_idt(&idt);
        svm_leave_to_guest(&frame, svm.ctx.virt().as_u64())
    }
}

unsafe fn load_gdt(dtr: &DescriptorTableRegister) {
    unsafe {
        asm!("lgdt [{}]", in(reg) dtr, options(nostack, preserves_flags));
    }
}

unsafe fn load_idt(dtr: &DescriptorTableRegister) {
    unsafe {
        asm!("lidt [{}]", in(reg) dtr, options(nostack, preserves_flags));
    }
}

unsafe extern "C" {
    fn svm_vmload(vmcb_pa: u64);
    fn svm_leave_to_guest(frame: &DevirtFrame, ctx: u64) -> !;
}
