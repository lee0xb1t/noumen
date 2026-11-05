//! TSC 选择性隐藏的 hypercall 实现（`0x200..=0x203`）。
//!
//! 目标配置是**全局**的（所有 vCPU 共享），每个 vCPU 在下次退出时把自己的
//! `rdtsc_exiting` 同步到最新配置，所以这里只写全局状态、不碰当前 VMCS。

use crate::{
    arch::x86_64::{
        guest_context::GuestContext, resume::EmulationResult, vcpu::Vcpu, vmx::vmcs,
        vmx::state::selective_tsc,
    },
    klog,
};

/// `0x200 TSC_SET_TARGET_PID`：`RDX = pid`，`R8 = 模式`，`pid == 0` 清除。
///
/// 模式：`0` = 只内核（CPL0）、`1` = 只用户（CPL3）、`2` = 内核 + 用户。
/// 只看某一个特权级时，另一个特权级连 PID 都不读，省掉每次退出的页表翻译。
pub fn set_target_pid(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    let pid = context.rdx as u32;
    let mode = context.r8 as u32;

    if mode > selective_tsc::TARGET_MODE_BOTH {
        klog!("TSC: bad target mode {}", mode);
        context.rax = 1;
        return Ok(());
    }

    selective_tsc::set_target_pid(pid, mode);

    context.rax = 0;
    Ok(())
}

/// `0x201 TSC_SET_TARGET_NAME`：`RDX = 名称 VA`，`R8 = 名称长度`。
///
/// 需要遍历 `PsActiveProcessHead`，而 `EPROCESS` 链表头地址不在 `KernelArgs`
/// 里（loader 现在只传字段偏移）。这里保留编号与参数约定，返回 `-1` 表示
/// 未找到，调用方改用 `0x200` 直接传 PID。
pub fn set_target_name(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    let name_va = context.rdx;
    let name_len = context.r8;

    if name_va == 0 || name_len == 0 || name_len > 15 {
        klog!(
            "TSC: bad name request (va={:#x}, len={})",
            name_va,
            name_len
        );
        context.rax = u64::MAX;
        return Ok(());
    }

    klog!("TSC: name lookup not implemented, use pid instead");

    context.rax = u64::MAX;
    Ok(())
}

/// `0x202 TSC_SET_TARGET_MODULE`：`RDX = 名称 VA`（保留）、`R8 = 基址`、`R9 = 大小`。
///
/// `R9 = 0` 清除目标模块。
pub fn set_target_module(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    let base = context.r8;
    let size = context.r9;

    // 模块范围必须落在内核半区，且大小不超过 256 MiB；否则一定是调用方写错了。
    if size != 0 && (base < 0xffff_8000_0000_0000 || size > 0x1000_0000) {
        klog!("TSC: bad module range base={:#x} size={:#x}", base, size);
        context.rax = 1;
        return Ok(());
    }

    selective_tsc::set_target_module(base, size);

    context.rax = 0;
    Ok(())
}

/// `0x206 TSC_SET_HIT_LOG`：`RDX` = `0` 关、非 0 开（默认开）。
///
/// 测量时必须关掉：一条命中日志要 12000~45000 tick，落在 `rdtsc … rdtsc` 的窗口
/// 里，除数 10 时直接给读数加上 1200~4500。
pub fn set_hit_log(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    selective_tsc::set_hit_log(context.rdx != 0);

    context.rax = 0;

    Ok(())
}

/// `0x205 TSC_SET_DIVISOR`：`RDX` = 除数 N（`1..=1000000`），默认 10。
///
/// 目标看到的时钟按
///
/// ```text
/// 可见值 += (物理值 − 上一次物理值) / N
/// ```
///
/// 推进。N 越大，目标量到的差值越小；`N = 1` 等于不减速。差值小于 N 时原样透出，
/// 免得把两次紧挨着的读抹成同一个值。
pub fn set_divisor(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    let divisor = context.rdx;

    match selective_tsc::set_divisor(divisor) {
        Ok(()) => context.rax = 0,
        Err(invalid) => {
            klog!(
                "TSC: divisor {} out of range (1..={})",
                invalid,
                selective_tsc::MAX_DIVISOR
            );
            context.rax = 1;
        }
    }

    Ok(())
}

/// `0x204 TSC_SET_EXIT_OVERHEAD`：`RDX` = 每次命中退出的补偿 tick，`0` 关闭。
///
/// 账本只能测到「上一次 exit → 本次 entry」这段 root 时间；guest→root 转换本身
/// （硬件在写 `exit_tsc` 之前那段）root 侧读不到，所以每次退出都会漏一截。漏的
/// 量就是一次转换的长度：目标进程量到的 `rdtsc; cpuid; rdtsc` 差值里，两次
/// `RDTSC` 之间夹着 **两次**退出（`cpuid` 和第二次 `RDTSC`），所以补偿值取
/// 「实测残差 / 2」。
///
/// 调用方先设好目标 PID，再让目标自己量残差，然后调这里。填 0 退回不补偿。
pub fn set_exit_overhead(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    let ticks = context.rdx;

    match selective_tsc::set_exit_overhead(ticks) {
        Ok(()) => context.rax = 0,
        Err(invalid) => {
            klog!(
                "TSC: exit overhead {} out of range (max {})",
                invalid,
                selective_tsc::MAX_EXIT_OVERHEAD
            );
            context.rax = 1;
        }
    }

    Ok(())
}

/// `0x203 TSC_SET_WIN_OFFSETS`：`RDX` = 结构 VA，`R8` = 字节数。
///
/// 结构就是 `KernelArgs.WinOffsets`（14 个 u32，56 字节，字段顺序一致）。
/// 用来在**虚拟化之后**换偏移表：`KernelArgs` 只在 `_start` 读一次，想让
/// 新 build 的系统也能做 PID 匹配就得靠这条。
///
/// `build == 0` 表示清除覆盖、退回 `KernelArgs` 里那份。
pub fn set_win_offsets(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    const STRUCT_BYTES: u64 = 56;

    let va = context.rdx;
    let length = context.r8;

    if va == 0 || length < STRUCT_BYTES {
        klog!("TSC: bad win_offsets request (va={:#x}, len={})", va, length);
        context.rax = 1;
        return Ok(());
    }

    let Ok(cr3) = vmcs::guest::Cr3::read() else {
        context.rax = 1;
        return Ok(());
    };
    let cr3 = cr3.as_raw();

    let mut fields = [0u32; 14];
    for (index, slot) in fields.iter_mut().enumerate() {
        let Some(value) = selective_tsc::read_guest_u32(cr3, va + (index as u64) * 4) else {
            klog!("TSC: win_offsets unreadable at {:#x}", va);
            context.rax = 1;
            return Ok(());
        };

        *slot = value;
    }

    let offsets = crate::kargs::WinOffsets {
        build: fields[0],
        list_blink: fields[1],
        eproc_link: fields[2],
        kproc_dtb: fields[3],
        eproc_pid: fields[4],
        eproc_name: fields[5],
        eproc_peb: fields[6],
        eproc_section_base: fields[7],
        eproc_exit_status: fields[8],
        eproc_thread_list: fields[9],
        eproc_wow64: fields[10],
        eproc_vad_root: fields[11],
        kthread_process: fields[12],
        reserved5: fields[13],
    };

    // build 和 PID 偏移是这个机制真正依赖的两项；缺任何一个都当非法输入。
    if offsets.build != 0 && (offsets.eproc_pid == 0 || offsets.kthread_process == 0) {
        klog!(
            "TSC: win_offsets rejected (build={}, pid_off={}, kthread_off={})",
            offsets.build,
            offsets.eproc_pid,
            offsets.kthread_process
        );
        context.rax = 1;
        return Ok(());
    }

    crate::hv::set_win_offsets_override(&offsets);

    if offsets.build == 0 {
        klog!("TSC: win_offsets override cleared");
    } else {
        klog!(
            "TSC: win_offsets override build={} pid=0x{:x} kthread=0x{:x}",
            offsets.build,
            offsets.eproc_pid,
            offsets.kthread_process
        );
    }

    context.rax = 0;
    Ok(())
}
