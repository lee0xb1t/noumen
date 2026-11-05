use crate::arch::x86_64::{
    cr,
    guest_context::{GeneralPurposeRegister, GuestContext},
    resume::{EmulationError, EmulationResult, Terminate},
    vcpu::Vcpu,
    vmx::{
        exit_info::{
            VmxExitQualification, VmxExitQualificationAccessType,
            VmxExitQualificationControlRegister, VmxExitQualificationMovCr,
        },
        instruction::{Invvpid, InvvpidType},
        vmcs,
    },
};

use super::skip_instruction;

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::{
    paging::MemoryType,
    vmx::{ept, instruction::Invept},
};

pub(super) fn handle_mov_cr(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let qualification = VmxExitQualification::read()?.mov_cr();

    match qualification.access_type() {
        VmxExitQualificationAccessType::MovToCr => {
            let gpr = mov_cr_operand(&qualification)?;

            match control_register(&qualification)? {
                VmxExitQualificationControlRegister::Cr0 => {
                    emulate_mov_to_cr0(vcpu, context, &gpr)?
                }
                VmxExitQualificationControlRegister::Cr3 => {
                    emulate_mov_to_cr3(vcpu, context, &gpr)?
                }
                VmxExitQualificationControlRegister::Cr4 => {
                    emulate_mov_to_cr4(vcpu, context, &gpr)?
                }
                _ => (),
            }
        }
        VmxExitQualificationAccessType::MovFromCr => {
            let gpr = mov_cr_operand(&qualification)?;

            match control_register(&qualification)? {
                VmxExitQualificationControlRegister::Cr3 => {
                    emulate_mov_from_cr3(vcpu, context, &gpr)?
                }
                _ => (),
            }
        }
        VmxExitQualificationAccessType::Clts => emulate_clts(vcpu, context)?,
        VmxExitQualificationAccessType::Lmsw => {
            emulate_lmsw(vcpu, context, qualification.lmsw_source_data())?
        }
    }

    Ok(())
}

/// A missing operand means the qualification does not match the exit reason.
fn mov_cr_operand(
    qualification: &VmxExitQualificationMovCr,
) -> EmulationResult<GeneralPurposeRegister> {
    qualification
        .general_purpose_register()
        .ok_or_else(|| EmulationError::Terminate(Terminate::internal()))
}

fn control_register(
    qualification: &VmxExitQualificationMovCr,
) -> EmulationResult<VmxExitQualificationControlRegister> {
    qualification
        .control_register()
        .ok_or_else(|| EmulationError::Terminate(Terminate::internal()))
}

fn emulate_mov_to_cr0(
    vcpu: &mut Vcpu,
    context: &mut GuestContext,
    gpr: &GeneralPurposeRegister,
) -> EmulationResult<()> {
    let mut new_cr0 = vmcs::guest::Cr0::from_raw(context.read_gpr(*gpr)?);

    let curr_cr4 = vmcs::guest::Cr4::read_effective()?;

    new_cr0.set_reserved1(0);
    new_cr0.set_reserved2(0);
    new_cr0.set_reserved3(0);
    new_cr0.set_extension_type(false);

    // https://www.felixcloutier.com/x86/mov-1#64-bit-mode-exceptions

    // #GP(0) if setting any reserved bits in CR0[63:32]
    if new_cr0.reserved4() != 0 {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if setting CR0.PG while CR0.PE is clear
    if new_cr0.paging_enable() && !new_cr0.protection_enable() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if invalid bit combination
    if new_cr0.not_write_through() && !new_cr0.cache_disable() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if an attempt is made to clear CR0.PG
    if !new_cr0.paging_enable() {
        return Err(EmulationError::gp(0));
    }

    if curr_cr4.control_flow_enforcement_enable() && !new_cr0.write_protect() {
        return Err(EmulationError::gp(0));
    }

    #[cfg(feature = "vmx-ept")]
    {
        // the guest tried to modify CR0.CD or CR0.NW, which must be updated manually
        let curr_cr0 = vmcs::guest::Cr0::read_effective()?;

        if (new_cr0.cache_disable() != curr_cr0.cache_disable())
            || (new_cr0.not_write_through() != curr_cr0.not_write_through())
        {
            // TODO: should we care about NW?

            // IMPORTANT: CR0.CD 影响**所有**核看到的缓存语义(MTRR 是全局的),
            // 所以同样只改本核的表 + bump 代数, 让别的核下次退出时自己跟上.
            let generation = ept::bump_memory_type_generation();
            let vmx_vcpu = vcpu.backend.as_vmx_mut();

            if new_cr0.cache_disable() {
                vmx_vcpu.ept.set_memory_type(MemoryType::Uncacheable);
            } else {
                vmx_vcpu.ept.update_memory_type();
            }

            Invept::single(vmx_vcpu.ept.eptp().as_raw());
            vmx_vcpu.ept_generation = generation;
        }
    }

    vmcs::control::Cr0Shadow::write_shadow(new_cr0.as_raw())?;

    let new_cr0 = new_cr0.as_raw() | vcpu.backend.as_vmx().cached.vmx_cr0_fixed0;
    let new_cr0 = new_cr0 & vcpu.backend.as_vmx().cached.vmx_cr0_fixed1;
    vmcs::guest::Cr0::new(new_cr0).write()?;

    skip_instruction()
}

fn emulate_mov_to_cr3(
    vcpu: &mut Vcpu,
    context: &mut GuestContext,
    gpr: &GeneralPurposeRegister,
) -> EmulationResult<()> {
    // CR3 切换不隐藏: 它是 Windows 上最频繁的退出, 隐藏它会累积出几 ms/s 的
    // 偏差, 而 hv 的模型只在“其它退出”上把这些偏差一次还掉, 频率高了就等于
    // 一直在还. 留着诚实退出, 正好充当回收点.
    let mut new_cr3 = vmcs::guest::Cr3::from_raw(context.read_gpr(*gpr)?);

    let curr_cr4 = vmcs::guest::Cr4::read_effective()?;

    let mut invalidate_tlb = true;

    if curr_cr4.pcid_enable() && (new_cr3.as_raw() & (1u64 << 63)) != 0 {
        invalidate_tlb = false;
        // CR3[63] 只是 no-flush 提示, 不是地址位, 写进 VMCS 之前必须清掉;
        // 直接 `set_raw(!(1u64 << 63))` 会得到 0x7FFF_FFFF_FFFF_FFFF.
        new_cr3.set_raw(new_cr3.as_raw() & !(1u64 << 63));
    }

    // a mask where bits [63:MAXPHYSADDR] are set to 1
    let reversed_mask = !((1 << vcpu.backend.as_vmx().cached.nr_phys_addr_bits) - 1);

    if (new_cr3.as_raw() & reversed_mask) != 0 {
        return Err(EmulationError::gp(0));
    }

    // CR3-load exiting 拦下了 guest 的 `mov cr3`, 硬件因此不会自己做 TLB
    // 失效. 旧翻译必须由这里主动刷掉, 否则 guest 会继续用已被释放/复用的
    // 页的老映射, 典型表现就是用户态堆损坏.
    if invalidate_tlb {
        Invvpid::new(
            InvvpidType::SingleContextRetainingGlobals,
            vcpu.backend.as_vmx().vpid,
            0,
        )
        .invvpid();
    }

    new_cr3.write()?;

    skip_instruction()
}

fn emulate_mov_to_cr4(
    vcpu: &mut Vcpu,
    context: &mut GuestContext,
    gpr: &GeneralPurposeRegister,
) -> EmulationResult<()> {
    let new_cr4 = vmcs::guest::Cr4::from_raw(context.read_gpr(*gpr)?);

    let curr_cr0 = vmcs::guest::Cr0::read_effective()?;
    let curr_cr4 = vmcs::guest::Cr4::read_effective()?;

    let curr_cr3 = vmcs::guest::Cr3::read()?;

    // https://www.felixcloutier.com/x86/mov-1#64-bit-mode-exceptions

    // #GP(0) if CR4.SMXE is set while CPUID.01H:ECX[6] (SMX) is clear.
    if !vcpu.backend.as_vmx().cached.smx_supported && new_cr4.smx_enable() {
        return Err(EmulationError::gp(0));
    }

    // CPUID.01H:ECX.VMX is cleared, so CR4.VMXE is a reserved bit. Reject it
    // instead of letting a detector create a software-visible VMX context.
    if new_cr4.vmx_enable() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if an attempt is made to write a 1 to any reserved bits
    if new_cr4.reserved1() || new_cr4.reserved2() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if an attempt is made to change CR4.PCIDE from 0 to 1 while CR3[11:0] != 000H
    if (new_cr4.pcid_enable() && !curr_cr4.pcid_enable()) && (curr_cr3.as_raw() & 0xfff) != 0 {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if CR4.PAE is cleared
    if !new_cr4.physical_address_extension() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if CR4.LA57 is enabled
    if new_cr4.linear_addresses_57_bit() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if CR4.CET == 1 and CR0.WP == 0
    if new_cr4.control_flow_enforcement_enable() && !curr_cr0.write_protect() {
        return Err(EmulationError::gp(0));
    }

    // invalidate TLB entries if required
    if (new_cr4.page_global_enable() != curr_cr4.page_global_enable())
        || (!new_cr4.pcid_enable() && curr_cr4.pcid_enable())
        || (new_cr4.smep_enable() && !curr_cr4.smep_enable())
    {
        Invvpid::new(InvvpidType::SingleContext, vcpu.backend.as_vmx().vpid, 0).invvpid();
    }

    vmcs::control::Cr4Shadow::write_shadow(new_cr4.as_raw())?;

    // make sure to account for VMX reserved bits when setting the real CR4
    let new_cr4 = new_cr4.as_raw() | vcpu.backend.as_vmx().cached.vmx_cr4_fixed0;
    let new_cr4 = new_cr4 & vcpu.backend.as_vmx().cached.vmx_cr4_fixed1;
    vmcs::guest::Cr4::new(new_cr4).write()?;

    skip_instruction()
}

fn emulate_clts(_: &mut Vcpu, _: &mut GuestContext) -> EmulationResult<()> {
    // clear CR0.TS in the read shadow
    vmcs::control::Cr0Shadow::write_shadow(
        vmcs::control::Cr0Shadow::read_shadow()? & !cr::Cr0::CR0_TASK_SWITCHED_FLAG,
    )?;

    // clear CR0.TS in the real CR0 register
    vmcs::guest::Cr0::new(vmcs::guest::Cr0::read()?.as_raw() & !cr::Cr0::CR0_TASK_SWITCHED_FLAG)
        .write()?;

    skip_instruction()
}

fn emulate_lmsw(_: &mut Vcpu, _: &mut GuestContext, value: u16) -> EmulationResult<()> {
    let new_cr0 = cr::Cr0::from_bits(value as u64);

    // update the guest CR0 read shadow
    let mut shadow_cr0 = cr::Cr0::from_bits(vmcs::control::Cr0Shadow::read_shadow()?);
    shadow_cr0.set_protection_enable(new_cr0.protection_enable());
    shadow_cr0.set_monitor_coprocessor(new_cr0.monitor_coprocessor());
    shadow_cr0.set_emulate_fpu(new_cr0.emulate_fpu());
    shadow_cr0.set_task_switched(new_cr0.task_switched());
    vmcs::control::Cr0Shadow::write_shadow(shadow_cr0.bits())?;

    // update the real guest CR0.
    // we don't have to worry about VMX reserved bits since CR0.PE (the only
    // reserved bit) can't be cleared to 0 by the LMSW instruction while in
    // protected mode.
    let mut real_cr0 = vmcs::guest::Cr0::read()?;
    real_cr0.set_protection_enable(new_cr0.protection_enable());
    real_cr0.set_monitor_coprocessor(new_cr0.monitor_coprocessor());
    real_cr0.set_emulate_fpu(new_cr0.emulate_fpu());
    real_cr0.set_task_switched(new_cr0.task_switched());
    real_cr0.write()?;

    skip_instruction()
}

fn emulate_mov_from_cr3(
    _: &Vcpu,
    context: &mut GuestContext,
    gpr: &GeneralPurposeRegister,
) -> EmulationResult<()> {
    context.write_gpr(*gpr, vmcs::guest::Cr3::read()?.as_raw())?;

    skip_instruction()
}
