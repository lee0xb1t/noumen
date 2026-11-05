use crate::{
    arch::x86_64::{
        guest_context::GuestContext,
        resume::{Resume, Terminate},
        vcpu::Vcpu,
        vmx::{
            exit_handler::dispatch_vm_exit,
            exit_info::VmxExitReason,
            injection,
        },
    },
    klog,
};

use x86::{
    msr,
    vmx::vmcs::{control, guest},
};

unsafe extern "C" {
    pub unsafe fn vm_exit();
}

unsafe extern "C" {
    pub unsafe fn vm_launch() -> bool;
}

#[unsafe(no_mangle)]
extern "C" fn handle_vm_exit_stub(context: &mut GuestContext) -> bool {
    let vcpu = unsafe { Vcpu::current() };

    // 别的核改过 MTRR / CR0.CD 时, 先补齐本核的 EPT 类型.
    // 然后再把其它核安装/移除过的 EPT hook 应用到本核: 两件事都必须在
    // 分发本次退出之前完成, 否则本核会用旧翻译处理 violation.
    #[cfg(feature = "vmx-ept")]
    {
        let cpu_id = vcpu.cpu_id;

        vcpu.backend.as_vmx_mut().sync_ept_memory_types();
        crate::arch::x86_64::vmx::ept::hook::sync(cpu_id, vcpu.backend.as_vmx_mut());
        crate::arch::x86_64::vmx::ept::hide_self::sync(cpu_id, vcpu.backend.as_vmx_mut());
    }

    let reason = match VmxExitReason::read() {
        Ok(reason) => reason,
        Err(e) => fatal(Terminate::Vmcs(e)),
    };

    match dispatch_vm_exit(context, &mut *vcpu, reason) {
        Resume::Terminate(reason) => fatal(reason),
        Resume::Stop => {
            devirtualize();
            true
        }
        Resume::Continue => {
            false
        }
        Resume::Inject(event) => {
            match injection::inject(event) {
                Ok(()) => {
                    false
                }
                Err(reason) => fatal(reason),
            }
        }
    }
}

/// `vmresume` 失败时会落到这条 Rust 入口（成功则永不返回）：报告指令错误后停机。
#[unsafe(no_mangle)]
extern "C" fn fatal_vmresume_error(error: u64) -> ! {
    klog!("\nFATAL: VMRESUME failed, instruction error = {:#x}", error);

    halt()
}

/// `lgdt`/`lidt` 的操作数: limit 紧跟 base, 不能有填充.
#[repr(C, packed)]
struct DescriptorTableRegister {
    limit: u16,
    base: u64,
}

/// 把 VMCS 里的 guest 系统状态装回 CPU，然后由 asm 走 `vmxoff` + `iretq`。
///
/// 顺序照 hv 的 `handle_vm_exit` 停止分支: CR0/CR4 shadow → DR7 → MSR → CR3 →
/// GDTR → IDTR → TSS → LDTR → 段选择子 → FS/GS base. asm 只负责 RIP/CS/
/// RFLAGS/RSP/SS/CR0/CR4 和最后那两条指令.
fn devirtualize() {
    unsafe {
        // 最后一步会把 FS base 换成 guest 的 (内核态 Windows 里是 0), 那之后
        // Vcpu::current() 就指向 0 了, 所以先把 cpu 号取出来.
        let cpu_id = Vcpu::current().cpu_id;

        // 被模拟的 VMREAD/VMWRITE 会留下脏状态位, 所以只看读回来的值.
        let rd = |field: u32| {
            let value: u64;
            core::arch::asm!(
                "vmread {0}, {1}",
                in(reg) field as u64,
                out(reg) value,
                options(att_syntax, nostack)
            );
            value
        };
        let wr = |field: u32, value: u64| {
            core::arch::asm!(
                "vmwrite {1}, {0}",
                in(reg) field as u64,
                in(reg) value,
                options(att_syntax, nostack)
            );
        };

        // asm 里 mov cr0/cr4 直接用这两个 shadow: host-owned 位保留, 其余取 guest 值.
        let cr0_owned = rd(control::CR0_GUEST_HOST_MASK);
        let cr0 = (rd(guest::CR0) & cr0_owned) | (rd(control::CR0_READ_SHADOW) & !cr0_owned);
        wr(control::CR0_READ_SHADOW, cr0);

        let cr4_owned = rd(control::CR4_GUEST_HOST_MASK);
        // VMXE 必须清掉: vmxoff 之后带着 CR4.VMXE 跑是 #GP.
        let cr4 =
            ((rd(guest::CR4) & cr4_owned) | (rd(control::CR4_READ_SHADOW) & !cr4_owned)) & !0x2000;
        wr(control::CR4_READ_SHADOW, cr4);

        let gdt_base = rd(guest::GDTR_BASE);
        let gdt_limit = rd(guest::GDTR_LIMIT) as u16;
        let idt_base = rd(guest::IDTR_BASE);
        let idt_limit = rd(guest::IDTR_LIMIT) as u16;
        let tr = rd(guest::TR_SELECTOR) as u16;

        // 日志必须在动手之前: lidt 一装上 guest 的 IDT, 这个核上的异常就归 guest 了,
        // 那种状态里再进 Rust 只会换一个更难看的 bugcheck.
        klog!(
            "devirt: cpu#{} gdt={:#x} idt={:#x} idt_limit={:#x} cr0={:#x} cr4={:#x}",
            cpu_id,
            gdt_base,
            idt_base,
            idt_limit,
            cr0,
            cr4
        );

        // limit 为 0 的 IDT 会让 guest 回去后的第一条中断 #GP -> #DF -> 三重故障.
        if idt_base == 0 || idt_limit == 0 {
            stop_failed("guest IDTR unusable");
        }

        core::arch::asm!("mov dr7, {}", in(reg) rd(guest::DR7), options(nostack));

        msr::wrmsr(msr::IA32_SYSENTER_CS, rd(guest::IA32_SYSENTER_CS));
        msr::wrmsr(msr::IA32_SYSENTER_ESP, rd(guest::IA32_SYSENTER_ESP));
        msr::wrmsr(msr::IA32_SYSENTER_EIP, rd(guest::IA32_SYSENTER_EIP));
        msr::wrmsr(msr::IA32_PAT, rd(guest::IA32_PAT_FULL));
        msr::wrmsr(msr::IA32_DEBUGCTL, rd(guest::IA32_DEBUGCTL_FULL));

        core::arch::asm!("mov cr3, {}", in(reg) rd(guest::CR3), options(nostack));

        let gdt = DescriptorTableRegister {
            limit: gdt_limit,
            base: gdt_base,
        };
        core::arch::asm!("lgdt [{}]", in(reg) &gdt, options(nostack));

        let idt = DescriptorTableRegister {
            limit: idt_limit,
            base: idt_base,
        };
        core::arch::asm!("lidt [{}]", in(reg) &idt, options(nostack));

        if tr != 0 {
            // TSS 描述符 busy(0xB) -> available(0x9), 否则之后的中断切栈会 #GP.
            let desc = (gdt_base + ((tr as u64 >> 3) * 8)) as *mut u8;
            *desc.add(5) = (*desc.add(5) & 0xF0) | 0x9;
            core::arch::asm!("ltr {:x}", in(reg) tr, options(nostack));
        }

        core::arch::asm!("lldt {:x}", in(reg) rd(guest::LDTR_SELECTOR) as u16, options(nostack));

        core::arch::asm!("mov ds, {:x}", in(reg) rd(guest::DS_SELECTOR) as u16, options(nostack));
        core::arch::asm!("mov es, {:x}", in(reg) rd(guest::ES_SELECTOR) as u16, options(nostack));
        core::arch::asm!("mov fs, {:x}", in(reg) rd(guest::FS_SELECTOR) as u16, options(nostack));
        core::arch::asm!("mov gs, {:x}", in(reg) rd(guest::GS_SELECTOR) as u16, options(nostack));

        // FS/GS base 要在选择子之后写: 装选择子会把 hidden base 换成描述符 base.
        msr::wrmsr(msr::IA32_FS_BASE, rd(guest::FS_BASE));
        msr::wrmsr(msr::IA32_GS_BASE, rd(guest::GS_BASE));
    }
}

/// Fail closed: root 里 `hlt` 且 IF=0, 这个核就废了.
fn fatal(reason: Terminate) -> ! {
    klog!(
        "\nFATAL: vCPU #{} stopped: {}",
        unsafe { Vcpu::current() }.cpu_id,
        reason
    );

    halt()
}

/// stop 收尾发现状态没法安全返回 guest: 记录原因后停机.
fn stop_failed(message: &str) -> ! {
    klog!("\nFATAL: vm_stop: {}", message);

    halt()
}

/// 这个核不再回 guest: 关中断后 `hlt`.
fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("cli; hlt", options(nostack)) };
    }
}
