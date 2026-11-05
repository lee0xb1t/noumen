// IDT use x86_64 lib

use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    arch::x86_64::{
        interrupt_stub::TrapFrame, safe_routine::HostExceptionInfo, vcpu::Vcpu,
        vendor::Vendor, vmx::vmcs::control::ProcBasedCtls,
    },
    klog,
};

/// Host NMIs seen while the root context is running. On SVM the root runs with
/// GIF=0, so these can only come from the tiny devirtualize window.
static HOST_NMIS: AtomicU32 = AtomicU32::new(0);

pub fn handle_host_interrupt(frame: &mut TrapFrame) {
    use x86_64::structures::idt::ExceptionVector;

    if let Ok(vec) = ExceptionVector::try_from(frame.vector as u8) {
        match vec {
            ExceptionVector::NonMaskableInterrupt => {
                let count = HOST_NMIS.fetch_add(1, Ordering::Relaxed) + 1;

                // NMI-window exiting is VMX-only. On AMD there is no window
                // control to arm (and no VMCS to read); just account for it.
                if Vendor::detect() == Vendor::Intel {
                    let vcpu = unsafe { Vcpu::current() };
                    vcpu.host.queued_nmis += 1;

                    if let Ok(mut proc_based) = ProcBasedCtls::read() {
                        proc_based.set_nmi_window_exiting(true);
                        let _ = proc_based.write_adjust();
                    }
                } else if count <= 4 {
                    klog!("SVM: host NMI while root (count={count})");
                }
            }
            _ => {
                if frame.r10 == 0 || frame.r11 == 0 {
                    panic!(
                        "Unhandled exception. Vector={}. RIP=0x{:x}. R10=0x{:x}. R11=0x{:x}.",
                        frame.vector, frame.rip, frame.r10, frame.r11
                    );
                }

                // r10 is exception handler address
                frame.rip = frame.r10;

                // r11 is exception info
                let host_ei = unsafe { &mut *(frame.r11 as *mut HostExceptionInfo) };
                host_ei.has_occurred = true;
                host_ei.vector = frame.vector;
                host_ei.error = frame.error;

                // prevent infinite exceptions
                frame.r10 = 0;
                frame.r11 = 0;
            }
        }
    }
}
