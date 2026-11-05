use super::*;
use super::super::state;
use super::page::{begin_single_step, write_page_state};
use super::support::*;
use crate::arch::x86_64::ldisasm;

pub fn handle_ept_violation(
    vmx: &mut VmxVcpu,
    qualification: &VmxExitQualificationEptViolation,
    gpa: u64,
    cpu_id: u32,
) -> EmulationResult<bool> {
    let page_gpa = gpa & !PAGE_MASK;

    let table = HOOKS.lock();
    let Some(index) = state::find_page_or_retired(&table[..], page_gpa) else {
        return Ok(false);
    };

    let entry = table[index].entry;
    let page_index = if entry.page_gpa == page_gpa { 0 } else { 1 };
    let fake_pfn = if page_index == 0 {
        entry.fake_pfn
    } else {
        entry.fake_pfn2
    };

    let current = match vmx.ept.pte_raw(PhysAddr::new(page_gpa)) {
        Ok(Some(pte)) => pte,
        Ok(None) => return Ok(false),
        Err(_) => return Err(EmulationError::Terminate(Terminate::internal())),
    };

    if entry.kind.is_breakpoint() && (!entry.active || !breakpoint_owner(cpu_id)) {
        write_page_state(vmx, &entry, 0, PteState::RealPassthrough)
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

        return Ok(true);
    }

    // **字节补丁模式**：页平时指向 clean 副本（R+W、X=0）。取指触发 violation 时
    // 把 leaf 换成真实页（R+W+X，里面有 `0xCC`），guest 重新取指执行 `0xCC`
    // → `#BP` → 我们的等待/自旋逻辑接手。数据访问本来就不该陷入（clean 页可读
    // 可写），真陷入也按“切到真实页”放行，避免死循环。
    if entry.byte_mode {
        write_page_state(vmx, &entry, 0, PteState::RealPassthrough)
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

        return Ok(true);
    }

    if entry.kind == HookKind::Hide {
        if page_index != 0 {
            return Ok(false);
        }

        let hide_state =
            state::classify_hide(current, page_gpa, vmx.hide_zero.pfn(), vmx.hide_shadow.pfn());

        let next = match hide_state {
            Some(PteState::HideData) if qualification.execute_access() => PteState::HideExec,

            Some(PteState::HideExec)
                if qualification.read_access() || qualification.write_access() =>
            {
                let rip = guest::Rip::read()
                    .map_err(|_| EmulationError::Terminate(Terminate::internal()))?
                    .as_raw();

                let rip_user = guest_is_user()
                    .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;
                let rip_cr3 = guest::Cr3::read()
                    .map_err(|_| EmulationError::Terminate(Terminate::internal()))?
                    .as_raw();
                let rip_gpa = guest_memory::translate_guest(
                    rip_cr3, rip, rip_user, false, false,
                )
                .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

                if vmx.hook_pending.is_some() {
                    PteState::HideData
                } else if (rip_gpa & !PAGE_MASK) == entry.page_gpa {
                    if !vmx.cached.monitor_trap_flag {
                        return Ok(false);
                    }

                    let offset = (rip & PAGE_MASK) as usize;
                    let mut bytes = [0u8; ldisasm::MAX_INSTRUCTION_LEN];
                    let available = (PAGE_SIZE - offset).min(bytes.len());

                    copy_from_gpa(
                        entry.page_gpa + offset as u64,
                        bytes.as_mut_ptr(),
                        available,
                    )
                    .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

                    let Some(instruction) = ldisasm::decode(&bytes[..available]) else {
                        return Ok(false);
                    };

                    if offset + instruction.len as usize > PAGE_SIZE {
                        return Ok(false);
                    }

                    vmx.hide_shadow.fill(0);

                    let destination =
                        unsafe { (vmx.hide_shadow.as_mut_ptr() as *mut u8).add(offset) };

                    copy_from_gpa(
                        entry.page_gpa + offset as u64,
                        destination,
                        instruction.len as usize,
                    )
                    .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

                    write_page_state(vmx, &entry, 0, PteState::HideShadow)
                        .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

                    begin_single_step(vmx, page_gpa, false, true, 0)?;

                    return Ok(true);
                } else {
                    PteState::HideData
                }
            }

            Some(PteState::RealPassthrough) => return Ok(true),

            _ => return Ok(false),
        };

        write_page_state(vmx, &entry, 0, next)
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

        return Ok(true);
    }

    let next = match state::classify(current, page_gpa, fake_pfn) {
        PteState::RealDataOnly
            if qualification.execute_access() && !qualification.ept_executable() =>
        {
            if entry.active {
                PteState::CopyExecOnly
            } else {
                PteState::RealPassthrough
            }
        }

        PteState::CopyExecOnly
            if (qualification.read_access() || qualification.write_access())
                && (!qualification.ept_readable() || !qualification.ept_writable()) =>
        {
            if entry.active {
                PteState::RealDataOnly
            } else {
                PteState::RealPassthrough
            }
        }

        PteState::RealPassthrough => return Ok(true),

        _ => return Ok(false),
    };

    write_page_state(vmx, &entry, page_index, next)
        .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

    Ok(true)
}

