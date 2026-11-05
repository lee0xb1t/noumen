# AGENTS.md

本文件是 noumen 的**当前实现说明**，也是新 agent 的入口。  
先读本文件，再读 [README.md](README.md)。

## 1. 项目是什么

noumen 是一个运行在 x86_64 上的 Type-1 风格 hypervisor：

- Rust `no_std`，编译为 `x86_64-unknown-none`；
- 当前由 Windows 内核驱动加载，驱动只负责分配内存、传 `KernelArgs`、调用入口；hypervisor 核心只依赖 x86_64 虚拟化扩展；
- 同时支持 Intel VMX/EPT 和 AMD SVM/NPT；
- 提供 EPT/NPT hook、EPT 原生断点、日志、HideSelf 和 `MEM_*` 内存服务；SVM 没有实现 NPT 原生断点。

**范围说明**：本文件只描述 noumen hypervisor 本身。Windows 驱动 `loader/` 和用户态调用方不在本文范围；只在 ABI 边界提到 `KernelArgs` 和 hypercall 的调用方。

## 2. 硬性规则

1. **不要引入对其他 hypervisor/项目的依赖或比较**。文档和注释只描述 noumen 自己的行为、约束和取舍。
2. **文档必须和代码一致**。改 ABI、feature、路径、测试数量、退出语义时，同时改本文件和相关文档。
3. **不要用全局 `#![allow(dead_code)]` 掩盖问题**。只允许在完整硬件 ABI 模块或明确保留的 API 上做局部 allow，并写清原因。
4. **不要把 `#[cfg(test)]` 当成消 dead_code 警告的工具**。生产 API 即使当前只有测试调用，也必须保留在非 test 构建中。
5. **不要修改 guest 真实物理页来做 hook**。所有代码 hook 都走 EPT/NPT 私有副本。
6. **任何 hook 路径都必须能在 `STOP`/devirt 前恢复**；否则 guest 会带着 `0xCC` 或 detour 字节继续运行。
7. **每个 crate 就地构建**：产物落在自己的 `target/`（`hypervisor/`、`um/uapp/`、`um/nsvr/`、`tools/rdtsc_probe/`）。仓库根 `target/` **只放暂存产物** `deviceio/`、`kdmapper/`；编译器临时目录走系统 temp（`%TEMP%/noumen-build`）。不要用 `.cargo-target`，也不要把 target 指到别的 crate。
8. **默认、release、`vmx-ept` only、`svm-npt` only、无 feature 五种配置都必须 0 warning 通过**。

## 3. 快速开始

```powershell
# 默认：logbuffer + panic 明细 + VMX EPT + SVM NPT + mem
cargo build
cargo build --release

# 单后端
cargo build --no-default-features --features vmx-ept
cargo build --no-default-features --features svm-npt

# 最小镜像
cargo build --release --no-default-features
```

完整构建（hypervisor + loader）用**仓库根**的脚本，产物按 loader 版本分开暂存：

```powershell
python build.py              # 两版都构建
python build_deviceio.py     # 只构建 deviceio 版
python build_kdmapper.py     # 只构建 kdmapper 版（顺带重生成 noumen_data.h）
python build.py --debug --skip-driver
python build.py --no-serial
```

产出：

```text
hypervisor\target\x86_64-unknown-none\<profile>\noumen   # hypervisor 镜像
target\deviceio\noumen, noumen_loader.sys, uapp.exe      # 暂存目录
target\kdmapper\noumen, noumen_kdmapper.sys, nsvr.exe
```

常用参数：

```powershell
python build.py --clean             # 先清中间产物再构建
python build.py --clean-only        # 只清理（cargo target + MSBuild build 目录），保留 target\<flavor>
python build.py --flavor kdmapper   # 只构建一版
python build.py --hypercall-key 0x2b3d   # nsvr 的默认 key（缺省 0x10F2C）
```

`--no-serial` 用独立的 `hypervisor\target\no-serial`，不会覆盖带串口的镜像。

## 4. Feature 矩阵

| feature | 默认 | 作用 |
| --- | --- | --- |
| `serial-console` | 否 | COM1 串口输出；需显式开启 |
| `buffer-log` | 是 | 512 条日志环（约 76 KiB）；loader 提供，或 `log_base == 0` 时从页池分配内部环 |
| `panic-detail` | 是 | panic 寄存器快照、KernelArgs、符号回溯 |
| `vmx-ept` | 是 | Intel 后端 + identity EPT + EPT hook/mem |
| `svm-npt` | 是 | AMD 的 identity NPT、NPF、NPT hook/mem、HideSelf |
| `mem` | 是 | `MEM_*` 通用内存服务；后端各自实现 |

关键规则：

- `mem` **不再隐含** `vmx-ept`。构建时若只要内存服务，必须显式同时给后端 feature。
- `POOL_PAGES`：开启 `vmx-ept` 或 `svm-npt` 时为 8192（32 MiB），否则 2048；页池不够时 EPT/NPT 初始化会失败。
  页池预算（`MAX_CPUS == 32`，最坏情况=2 MiB 模式覆盖 64 GiB）：每 vCPU 约 79 页（1 PML4 + 1 PDPT + 64 个 PD 表 + 1 dummy + 12 页 VMXON/VMCS/MSR bitmap/hide/host），32 核约 2528 页；宿主页表 514 页，内部日志环 20 页，合计约 3062 页；hook 拆表页和 fake/trampoline 副本再预留 2048 页，峰值约 5110 页（20 MiB）。1 GiB 模式每 vCPU 约 3 页（1 PML4 + 1 PDPT + 1 dummy），混合 MTRR 段每段再加 1 张 PD 表。NPT 同样按 `min(2^MAXPHYADDR, 1 TiB)`（1 GiB 叶）或 64 GiB（2 MiB 叶）覆盖，每 vCPU 约 3–66 页。
- **CET**：VMX 用 VM-exit bit 28 `load_ia32_cet_state` + VM-entry bit 20 `load_cet_state` 虚拟化 `IA32_S_CET`/`SSP`/`IA32_INTERRUPT_SSP_TABLE_ADDR`；另外五个没有 VMCS 字段的 CET MSR（`IA32_U_CET`、`IA32_PL0..PL3_SSP`）**透传**（见 §9.6）。两个控制位读回不生效或加载时 CET 未打开时，`emulate_cpuid` 会屏蔽 CET。SVM 的 VMCB 保存区本来就有 `s_cet`/`ssp`/`isst_addr`，由 VMRUN/#VMEXIT 自动换入换出（见 §10.2）。
- `vmx-ept` 关闭时，VMX 仍可编译（guest 直接跑宿主页表），但 EPT hook/mem 代码不编译；相关 hypercall 返回 `RAX = 3`。
- `svm-npt` 关闭时：SVM 核心仍编译（VMCB、VMRUN、CPUID/MSR、异常、devirt、ping/log/Stop），只关闭 `svm/npt`、NPF、NPT hook/mem、HideSelf 和 `NP_ENABLE`；guest 运行在宿主页表上，NPT 相关 hypercall 返回 `3`。共享 `Hypercall` enum 仍保留 `0x30..0x3a` 编号（ABI 不变）。`tests/svm-core-tests` 仍会把纯 SVM 模块按路径编进宿主测试，和主二进制的 feature 无关。
- `buffer-log` 关闭时，`klog!` 的调用点仍在（参数参与类型检查），但不会写日志。
- `buffer-log` 的环有两种来源：loader 通过 `KernelArgs.log_base/log_size` 提供；或 `log::init(0, _)` 从页池分配 hypervisor 自有的内部环（当前 20 页 = 77872 字节），此时 `log_size` 被忽略，guest 用 `FLUSH_LOGS` 取走记录。内部环要求页池已初始化，所以 `memory::init()` 必须在 `log::init()` 之前。
- `serial-console` 和 `buffer-log` 都关闭时，`klog!`/`klog_verbose!`/`klog_try!` 展开为 `if false { let _ = format_args!(...); }`，用于保留参数使用、避免 feature 构建的 unused 警告。
- 这个死分支仍会**类型检查参数**：只在某个 `cfg` 下存在的名字（例如调试用的 `mtrr`）必须让调用点和 import 使用同一个 `cfg`，否则 release/无日志构建会报未解析名称。

## 5. 启动链

```text
Windows driver
  → 分配 nonpaged 内存、构造 KernelArgs v4
  → 调用 noumen 入口
  → 校验 KernelArgs（版本、CR3、物理区间、镜像/PFN 表、日志区）
  → 初始化内存池（`buffer-log` 的内部环从这里分配）
  → 初始化日志/panic（`log_base == 0` 时分配内部环）
  → 初始化 host 状态
  → 按 CPU vendor 选择 VMX 或 SVM
  → 每个 CPU 建立 Vcpu、进入虚拟化
```

### KernelArgs v4

- 大小 **168 字节**，`repr(C, packed(8))`；
- 首字段 `version` 必须是 `4`；
- 只包含内存层字段 + 一张 Windows 结构偏移表，不含 OS/进程/模块/PE 语义；
- 关键字段：`system_cr3`、`system_cr3_virt`、`kernel_base`、`sym_base`、`str_base`、`log_base`、`kernel_pfn_base/count`、`phys_map_base/count`；
- **v4 新增 `win_offsets`**（`WinOffsets`，56 字节）：`build` + `_EPROCESS`/`_KTHREAD` 字段偏移，由 uapp 按 build 号选表后经驱动 `SetWinOffsets`(15) 下发，`Start` 时合并进来。`build == 0` 表示没提供，**CPL3 的 PID 匹配会全部失效**（那是唯一来源，已经没有 TEB 退路，见 §9.8 匹配规则）。`kthread_process` 是 **KTHREAD 相对**偏移；表由仓库根 `tools/gen_win_offsets.py` 生成（memflow 的 offsets/*.toml + ntoskrnl.exe 导出反汇编）；
- 读 packed 字段时先拷到局部变量，避免未对齐引用；
- `validate()` 在日志初始化前运行，不能访问 guest 内存。

## 6. 代码地图

```text
src/
  main.rs                 入口、CPU 初始化、后端选择
  hv.rs                   hypervisor 全局状态/参数
  kargs.rs                KernelArgs v4（含 WinOffsets）
  error.rs                HvError / 结果类型
  panic.rs                panic 报告
  log.rs                  klog!/klog_verbose!/klog_try! 与日志环接口
  log/buffer.rs           日志环实现（loader 环 / 页池内部环）
  arch/x86_64/
    memory.rs             物理页池、POOL_PAGES
    paging.rs             直接映射/页表位
    guest_memory.rs       guest VA→PA、copy_to/from_guest
    hypercall.rs          共享 hypercall ABI（只有编号/参数）
    vcpu/                 Backend enum、每 CPU Vcpu
    vmx/                  Intel 后端
    svm/                  AMD 后端
    hook_events.rs        hook 命中事件环（两个后端共用）
    ldisasm.rs            最小 x86 指令长度解码（detour stolen bytes）
    tsc.rs                TSC 频率探测
    serial.rs             COM1/no-op 串口
```

### VMX 代码

```text
vmx/
  state/                   VMXON、VMCS、controls、host/guest 状态
  exit_handler/            exit 分发、异常、CR、MSR、CPUID、EPT、TLB
  vmcs/                    完整 VMCS 字段访问器
  ept/
    hook/                  EPT hook 状态机、安装、命中、断点、EPT 原生断点
    mem/                   MEM_* 的 EPT 视图
    hide_self.rs           HideSelf
  hypercall/
    mod.rs                 VMCALL 分发
    hook.rs                EPT hook 命令
    wait.rs                EPT 原生断点等待
    diag.rs                ping/日志/HideSelf/ReadSelf
    mem.rs                 MEM_* 命令
  stub.rs                  VMX 进入/退出汇编边界
```

### SVM 代码

```text
svm/
  state/                   VMCB、controls、setup、init
  exit_handler/            VMRUN 退出分发、CPUID/MSR/NPF/异常
  vmcb/                    完整 VMCB 字段访问器
  npt/
    hook/                  NPT hook（byte-patch + NPT function hook）
    mem/                   MEM_* 的 identity NPT 视图
    hide_self.rs           HideSelf
    state.rs               NptView：OriginalExec/ShadowNx/ShadowExec
    tlb.rs                 TLB 失效
  hypercall/
    mod.rs                 VMMCALL 入口、key 校验、Stop/RIP
    command.rs             命令族路由
    hook.rs                NPT hook 命令
    wait.rs                等待协议（无 NPT 原生断点生产路径）
    diag.rs                ping/日志/HideSelf/ReadSelf
    mem.rs                 MEM_* / scan
  stub.rs                  VMRUN/#VMEXIT 汇编边界
```

## 7. Backend 抽象

`Backend` 是 VMX/SVM 的枚举，`Vcpu` 持有：

```text
Vcpu {
  backend: Backend,
  cpu_id: u32,
  ...
}
```

规则：

- 后端相关代码通过 `as_vmx()` / `as_vmx_mut()` / `as_svm()` 进入；
- 共享层不能依赖 `vmx::vmcs` 或 `svm::vmcb` 的具体字段；
- SVM 专属字段（如 `npt_shadow_view`）只放在 `SvmVcpu`，不要塞进共享 `Vcpu`；
- 后端 feature 关闭时，相关 accessor/模块必须 `cfg` 掉或返回 `3`。

## 8. Hypercall ABI

### 寄存器约定

```text
RAX = loader 提供的 key
RCX = command
RDX = arg0
R8  = arg1
R9  = arg2
R10 = arg3（少量命令）
RAX = 返回值
```

未知 key：

- VMX：注入 `#UD`；
- SVM：注入 `#UD`。

未知/后端不支持的 command：

- VMX：`RAX = 3`（少数路径返回 `#UD`）；
- SVM：`RAX = 3`。

### 分层

```text
arch/x86_64/hypercall.rs        共享 ABI：Hypercall enum、from_raw、PONG_MAGIC
vmx/hypercall/                  VMX 命令实现
svm/hypercall/                  SVM 命令实现
```

公共文件只允许放：

- 编号；
- 参数/返回约定；
- `from_raw`；
- 与后端无关的常量。

**不要**把某个后端的 handler、`Vcpu`、EPT/NPT 类型放进公共 ABI 文件。

### 入口

VMX：

```text
EXECUTE_VMCALL
  → vmx::exit_handler::misc::emulate_vmcall
  → vmx::hypercall::dispatch
  → 成功后 skip_instruction
  → Stop：先推进 RIP，再 VMXOFF
```

SVM：

```text
VMMCALL
  → svm::exit_handler::dispatch
  → svm::hypercall::dispatch
  → 内部 key 校验、命令分发、advance_rip
  → Stop：先推进 RIP，再 devirtualize
```

RIP 推进只能有一个所有者：

- VMX：exit handler 推进；
- SVM：SVM hypercall dispatch 推进。

不要在 handler 内再次推进，否则 guest 会跳过/重执行指令。

### 命令族

- `1..=4`：日志、ping、Stop；
- `5..=7`、`9..=11`、`13..=22`、`0x100`：hook/EPT 原生断点/自省；SVM 没有 NPT 原生断点路径；
- `0x20..=0x24`：`MEM_*`；
- `0x200..=0x206`：TSC 选择性隐藏（`TscSetTargetPid` / `TscSetTargetName` / `TscSetTargetModule` / `TscSetWinOffsets` / `TscSetExitOverhead` / `TscSetDivisor` / `TscSetHitLog`，见 §9.8）；
- `0x30..=0x3a`：SVM 专属命令（VMX 返回 `3`）。

## 9. VMX 关键点与坑

### 9.1 VMCS / host 状态

- VMCS host 区的 `FS_BASE` 指向当前 `Vcpu` 本身；`Vcpu` 一旦建立**不能移动**，否则 `Vcpu::current` 和 host 相对访问会失效。
- 每个 CPU 的 VMXON/VMCS 区域必须 4 KiB 对齐；页面池按 4 KiB 分配。
- `VM_EXIT_MSR_LOAD/STORE` 区域、MSR bitmap、EPT pointer 都是物理地址；必须来自 nonpaged/identity 可访问内存。
- **读写 CR0/CR4 只能用 `cr::Cr0`/`cr::Cr4` 的裸 `mov` 实现**：`x86::controlregs::cr4()` 内部是 `from_bits_truncate`，会把 flags 里没定义的位（典型是 CET bit23）静默清掉；`enable_vmx()` 再把它写回，就真的把 CET 关掉了（表现为 loader 里 `__readcr4()` 是 `0xb50ef8`，noumen 里读到的却是 `0x350ef8`）。
- `CR3` 不拦截，靠 VPID 区分地址空间；hook 页的 EPT 状态变化必须对当前核做 `INVEPT`/TLB 失效。
- **CR0/CR4 guest/host mask 里为 1 的位是 host-owned**（KVM 里 `CR4_GUEST_HOST_MASK = ~cr4_guest_owned_bits`，VM entry 时这些位取 HOST_CRx 的值，guest 写它们只改 read shadow）。**不要用全 1 掩码**：那会把 guest 的真实 CR4 永远钉在 HOST_CR4 上，CET/SMEP/SMAP/PCIDE 全都改不了，guest 执行 CET 指令直接 `#UD`，表现为线程启动 `STATUS_ILLEGAL_INSTRUCTION` → `0xEF`。debug 和 release 都必须用 `vmx_crX_fixed0 | !vmx_crX_fixed1`（CR0 另加 CD/WP）。CR4 read shadow 里清掉 VMXE，HOST_CR4 仍保留 VMXE 供 root 执行 VMX 指令。

### 9.2 EPT hook

核心不变量：

1. guest 真实物理页永远不写 `0xCC`/detour；
2. 执行视图和目标数据视图分离；
3. fake 页是 **execute-only**（EPT 读位关闭），任何数据读必须落到真实页或专门的数据页；
4. 入口跳转用 `mov r11, imm64; jmp r11`，**不要用** `jmp qword ptr [rip+0]` 之类的内存操作数跳转，因为 execute-only 页不能读操作数；
5. 被覆盖的 stolen 字节写入 origin/trampoline，末尾写回 `target+stolen`；
6. `stolen_len` 由 `ldisasm` 计算，必须覆盖完整指令；跨页时拒绝安装；
7. detour 的绝对跳转是 14 字节（`49 BB ... 41 FF E3`），不要按 12/13 字节算；
8. 所有核都应用新 EPT 状态后，才能释放 origin 页/复用槽位（`HOOK_RETIRED`）；
9. `HOOK_APPLY` 只影响当前核；调用方必须在所有核上调用后再释放旧页；
10. `STOP` 前恢复所有 hook 的真实页内容，否则 guest 带着补丁继续跑会 `0x3B`/`0xCC`。

状态机（概念）：

```text
Free → InFlight → Active → Closing → Retired → Free
```

- `InFlight`：已分配但尚未在所有核生效；不能命中；
- `Stale`/旧 generation：落后核必须先 `sync` 到最新代，不能直接读新页；
- `Retired`：所有在线 vCPU 都确认退役后才能复用。

### 9.3 EPT 页状态

- `CopyExecOnly`：执行副本，读位关闭，写位关闭；
- `RealDataOnly`：真实页，读/写打开，执行关闭；
- 数据读命中 fake 页会触发 EPT violation；根据 access 类型切到正确视图；
- MTF 用来单步一条指令；`#BP`/MTF 期间不能被别的 exit 打断状态机；
- 非 MTF 的 exit 如果发生在单步窗口内，必须 abort 单步并重新武装 one-shot。

### 9.4 HideSelf

- 隐藏 noumen 镜像的物理页：VMX 把 guest 读/写/取指都导向本核 dummy 页；SVM 指向 `ShadowNx` 零页（NX），取指会在 NPT 上触发异常，不会执行真实镜像；
- guest 的写只落在 dummy/零页，不修改真实镜像；guest 写入后能读到自己的写，但读不到 hypervisor 代码；
- 需要隐藏整个镜像分配，包括 PFN 表；
- root（host）仍可通过宿主页表读真实页；
- `KernelArgs.kernel_pfn_base/count` 由 loader 填，hypervisor 只消费；
- 隐藏页的 EPT/NPT 状态变化同样要所有核同步。

### 9.5 EPT mem

- `mem::available()` 只在 VM-exit 上下文为真；
- 直接映射只存在于 host 页表，guest 的 CR3 下访问会错，所以必须用当前 guest CR3 翻译；
- 所有 `MEM_*` 失败通过 `RAX = MemError` 返回，不注入 `#PF`；
- `MemScan` 的请求结构在 guest 内存，先 `read_struct`，扫描后 `write_struct` 回写；
- `MemMap` 只输出物理区间，不暴露 host 虚拟地址。

### 9.6 CET（影子栈）

- VMX 不自动保存 XCR0/CET。不开 "load CET state"/"load IA32_CET state" 时，VM exit 不保存 guest 的 `SSP`/`IA32_S_CET`，用户态 `ret` 会在影子栈上失配 → `#CP` → Windows 转成 `FAST_FAIL_CONTROL_INVALID_RETURN_ADDRESS`（`0xC0000409`）；critical process 因此退出 → `0xEF`；
- 控制位：VM-entry bit 20 `load_cet_state`、VM-exit bit 28 `load_ia32_cet_state`；字段：guest `S_CET`/`SSP`/`INTR_SSP_TABLE` = `0x6828/0x682a/0x682c`，host 对应 `0x6c18/0x6c1a/0x6c1c`（`x86` crate 没有这些常量，定义在 `vmcs/guest.rs`、`vmcs/host.rs`）；
- root 侧必须关闭 supervisor 影子栈：`HOST_S_CET = HOST_SSP = HOST_INTR_SSP_TABLE = 0`。否则 noumen 在 root 里的 `call`/`ret` 会消费 guest 的影子栈；
- `configure_cet_state()` 只有在两个控制位**读回都生效**时才置 `cached.cet_virtualized`；否则 `emulate_cpuid` 对 leaf 7 清 `CET_SS`（7.0:ECX[7]）和 `CET_U`/`CET_S`（7.1:EAX[6:7]），让 Windows 不启用影子栈；
- VMX 没有 `IA32_U_CET`/`IA32_PL0_SSP`/`IA32_PL1_SSP`/`IA32_PL2_SSP`/`IA32_PL3_SSP` 的 VMCS 字段，但 VM exit/entry 不会改动这些 MSR，所以**透传**（不拦截）；拦截它们会让 guest 每次 syscall 写 `PL3_SSP` 都产生 VM exit；
- `#CP`（vector 21）进异常位图，原样注入回 guest；日志只保留限流后的 `Unhandled` 类记录，不再打印 CET 快照；
- **只有加载时 CET 已经打开（`CR4.CET=1`）才接管/暴露 CET**：`cet_virtualized = cet_active_at_setup`，它同时是 `emulate_cpuid` 屏蔽 CET 的开关。加载后才打开 CET 时，已经在跑的线程没有影子栈条目，下一次 `ret` 就 `#CP`（`FAST_FAIL_CONTROL_INVALID_RETURN_ADDRESS`）；
- 开了 load-CET 后，guest 的 `IA32_S_CET`/`IA32_INTERRUPT_SSP_TABLE_ADDR` 每次 entry 都从 VMCS 字段加载，所以这两个 MSR 的 `WRMSR` 必须拦截（`setup_msr_bitmap`）并写字段，不能写真实 MSR；`RDMSR` 不拦截（entry 已把 MSR 装成字段值）；
- `IA32_U_CET`/`IA32_PL0_SSP`/`IA32_PL3_SSP` 透传给 guest，root 不使用它们；
- setup 时用 `RDSSPQ` 读当前 SSP，但只在真实 `CR4.CET=1` 时执行；CET 未开时 guest 字段写 0，下一次 exit 由硬件写入实际值；
- noumen 晚于 Windows 打开 CET 启动时也能接管：先把当前 `S_CET`/`SSP`/表地址写进 guest 字段，之后硬件随每次 exit 更新。

### 9.7 APERF/MPERF

- `IA32_MPERF`(0xE7)/`IA32_APERF`(0xE8) 是 per logical processor 的物理计数器，VMCS 里**没有固定字段**；想让硬件自动换入换出，只能把它们放进 VM-exit MSR-store / VM-entry MSR-load 列表。**本实现没有这么做**，改用软件按需同步（原因见下一条）；
- 为什么不用 MSR 列表：store 在退出时把 guest 值写进区域，load 在进入/退出时装回，但**列表里的值本身要在 root 里持续维护**（host 的 `APERF`/`MPERF` 也得跟着真实时间推进），成本与软件同步相同，还多两次记录写入。而且这两个计数器只有被读时才有意义，没有“每退出必须同步”的语义；
- 直接放行会让 guest 读到包含 host 执行时间的值；
- 安装条件：`CPUID.06H:ECX[0]`（硬件协调反馈）为 1；MSR bitmap 同时拦截**读和写**（`setup_msr_bitmap` 调 `perf.init()`）；
- `VirtualPerfCounters` **按需同步**（`vmx/state/perf.rs`）：
  - `ENABLE_PERF`（`state/mod.rs`）默认 **true**；`setup_msr_bitmap` 里 `perf.init()` 探支持并记首个基准；
  - guest `RDMSR` 调 `perf.value(msr)` → 先 `sync()`：读硬件计数器，把「上次基准以来的增量」加进 guest 视图，再重记基准；增量不是常量，就是两个计数器各自的差值；
  - guest `WRMSR` **保留写入值**（`perf.set`），绝不写真实 MSR；
  - **退出路径不再碰它**：`dispatch_vm_exit` 里没有 `accumulate()`/`mark_entry()`，不读这两个 MSR 时零开销；
- 代价：guest 每读一次这两个 MSR 产生一个 VM exit（只有这时才 `rdmsr`）；不支持该特性的 CPU 上整套逻辑不安装；
- 两次同步之间的 root 时间会算进增量（每次读最多多算一个 handler 的时长，之后不累积）；这是时间切片模拟，不等同硬件虚拟计数器。

### 9.8 TSC（选择性隐藏，`vmx/state/selective_tsc.rs`）

- **全系统 TSC 虚拟化已经移除**：`use_tsc_offsetting` + per-vCPU `TSC_OFFSET` 扣减、`VirtualTsc`、`IA32_TSC` 的 RDMSR/WRMSR 拦截都不再存在（**MSR-store 保留**，但只用来取退出时刻，见下）。原因见下一条；
- **为什么不能用 `TSC_OFFSET`**：它是 **per-vCPU** 的。12 个核各自扣减自己那段 host 时间，累积后同一物理时刻不同核读到的 TSC 能差几十毫秒；Windows 的 HAL 定时器跨核比较时直接 **`0x5C HAL_INITIALIZATION_FAILED`**（`Arg1=0x113`, `Arg2=0x27`，栈是 `HalpTimerDpcRoutine`）。想让 offset 全局一致必须 IPI 同步所有核的 VMCS，成本与竞态都不划算；
- **现在的做法：只对目标进程/模块隐藏**。`RDTSC_EXITING` 是 VM 级控制位、没法按进程开，所以只要存在隐藏目标就全系统拦 `RDTSC`，**在退出处理里再决定要不要扣**：
  - 每个 vCPU 只记**退出基准**（`last_exit_tsc` / `last_thread`，用于日志诊断），**虚拟时钟是全局唯一的**：`CLOCK_PHYS`/`CLOCK_VISIBLE` 两个原子量 + 一把短自旋锁（`CLOCK_LOCK`，root 里不能拿阻塞锁）。早先按线程键（KTHREAD/TEB）分槽，真机踩了两个坑：① 键不稳定（同一个进程先后出现 `0xac256ab000` / `0xac256ad000` 两个 "TEB"）；② 键一变就落到新槽、`last_phys` 归零，这一次直接返回物理值、下一次又回到慢速曲线，两个尺度相差「运行时长 × 90%」，样本于是出现 ±1e9 的正负跳变。现在只有一个目标、一条曲线：多线程共用同一个虚拟时钟，推进量只取两次命中之间的物理流逝，与哪个线程读的无关；换目标时 `reset_ledgers()` 整体清零；
  - **目标判定每次退出只算一次**：`dispatch_vm_exit` 调 `tsc::match_target()`，结果 `set_match()` 进 vCPU，`emulate_rdtsc`/`emulate_rdtscp` 只 `current_match()` 复用。早先两处各算一次，于是同一次退出既把 guest 页表走两遍（KPCR→KTHREAD→EPROCESS 三跳，每跳一次软件页表翻译）；
  - **退出时刻用硬件 VM-exit MSR-store**（`VMEXIT_MSR_STORE_COUNT = MSR_STORE_ENTRIES = 2`，`ADDR` 指向 `VmxVcpu::msr_store` 这一页）：**两条记录**——① `IA32_TSC`，硬件在退出转换过程中就写好了，比 handler 里再 `rdtsc` 早一个 handler 序言（几百 tick）；② `IA32_KERNEL_GS_BASE`，给 CPL3 的 PID 匹配当 KPCR 用（见下）。读回 `read_count() != 2`（有的嵌套 VMM 会清掉）时退回 handler 里的 `rdtsc`/`rdmsr`，日志打 `TSC: VM-exit MSR-store was cleared`；
  - **`IA32_KERNEL_GS_BASE` 不属于 VMCS 的 guest/host 状态**：VM 进出不会自动换它，只有显式 MSR-load 列表才会动。本 hypervisor 没有为它配 load 列表，root 里也从不 `wrmsr`/`swapgs`，所以 guest 的 `swapgs` 改的就是真实寄存器 —— 在 root 里 `rdmsr(0xC0000102)` 读到的**就是 guest 的值**（CPL3 时是 KPCR，CPL0 时是该线程的用户 TEB）。为了不依赖这个隐含约定，退出时让硬件把它的快照存进 MSR-store 区域，`kernel_gs_base()` 优先用快照、读不到才回退 `rdmsr`；

  - **只累加目标相关的窗口**：`matched` 由 `tsc::match_target()` 在同一个退出里判定，假就只更新基准、不累加。否则账本会把本核**所有** host 时间都扣掉——实测目标进程的时钟只到墙钟的 0.4%，超时/定时器全部失真；
  - 记账顺序是「先 `note_exit` 再处理退出」，因此 `hide()` 用的是**本次退出转换之前**就已完成的窗口，`RDTSC` 自身 handler 的时间不会被算进读数值；两次 `RDTSC` 之间夹着的其他退出（中断、别的进程的系统调用）只有 `matched` 时才会被扣掉；
  - **但账本漏掉「guest→root 转换」本身**：`exit_tsc` 是转换完成时硬件写的，从最后一条 guest 指令到那一刻的耗时 root 侧读不到。推导一下 `rdtsc; cpuid; rdtsc` 的差值：两次读之间发生了 **两次** exit（`cpuid`、第二次 `rdtsc`），handler 时间和「上一次 exit → 本次 entry」窗口都被扣干净，剩下的正好是这两次转换之和 `Δ_cpuid + Δ_rdtsc`。实测残差 10609 tick ⇒ 单次转换约 5300 tick（该值随机器/是否嵌套 VMware 变化，不能写死）；
  - **补偿**：`0x204 TSC_SET_EXIT_OVERHEAD`（`RDX` = 每次**命中退出**要补的 tick，`0` 关闭，上限 `MAX_EXIT_OVERHEAD = 1000000`）在 `note_exit` 里对每个 `matched` 退出累加一次，于是两次读之间补两次、正好抵消。这个值只能实测：先设目标 PID，让目标量 `rdtsc; cpuid; rdtsc` 的平均值（就是残差），取**一半**填进去。它按「命中退出」统一计，不做按退出类型区分，因此只是近似——但 pafish 那类检测量的正是「两次指令模拟退出」，近似项刚好对上。**填过头会让被隐藏进程的 TSC 回退**（每次退出补得比实际转换还多），`rewind=` 就是回退次数，不为 0 就把值调小；
  - **目标模式**（`0x200` 的 `R8`）：`0` = 只内核（CPL0）、`1` = 只用户（CPL3）、`2` = 都算（默认）。模式不允许的特权级**连 PID 都不读**（`TargetSnapshot::allows_cpl`），因为那是每次退出三跳 guest 页表翻译。非法模式返回 `RAX = 1`，不改变现有配置；
  - **只扣 root 段，guest 段一律如实透出**：`note_exit` 只把 `entry - last_exit`（上一个 handler）记进 `last_thread` 的账，`exit - entry`（guest 执行）不扣。**不要**再给「未命中的周期」把整段 guest 时间扣到所有目标线程头上（早期「单特权级模式 = CPU 时间语义」的做法）：① TSC 是**墙钟**不是 CPU 时间，线程被换出时真实 TSC 照样走；② 多核下会**重复累加**——每个核上非目标退出的间隙都加到所有目标线程账上，12 核跑满时每过 1 秒就记 12 秒，账本永远追不上物理时间，可见值被永久钳死（真机现象：样本从几千变成**全 0**，时钟不动了）。删掉那段逻辑后 `hide_guest_gap`/`guest_gap_hidden` 都不存在了；
  - **窗口起点一律用 handler 自己的 `rdtsc`，不信硬件 MSR-store 的值**：`note_exit` 里 `last_exit_tsc = handler_tsc`，于是窗口 `entry - last_exit` = 「上一个 handler 从开始执行到即将 vmresume」这一段，**必然落在 handler 内部**。MSR-store 的 `IA32_TSC` 在嵌套/被 host 抢占时可能比真实退出早很多（甚至是一两个退出之前的值），拿它当起点会把不属于 handler 的时间一起扣掉——真机表现是样本在「0」和「约 25000」之间乱跳：多扣一点被单调钳制压成 0，整个窗口漏掉就冒出 +25000（等于那条命中日志的 handler 时间）。少算的只有 guest→root 那次转换（~150 tick），由 `0x204` 补；
  - **root 窗口不设上限**：`entry - last_exit` 这段全部在 root 里（handler，或者 root 被 host 抢占），都是目标不该看到的时间；
  - `emulate_rdtsc`/`emulate_rdtscp` 判定顺序：`CPL==3` 且配了 PID → 走内核结构链取进程号比较；`CPL==0` 且配了模块 → 比较 `RIP ∈ [base, base+size)`；都不匹配就**原样返回物理 TSC**；
  - **可见值改成按比例放慢（不再做加减法）**：`hide()` 现在是

    ```text
    可见值 = 上一次可见值 + (物理值 − 上一次物理值) / TSC_SLOWDOWN_DIVISOR   (默认 10)
    ```

    只存两个数：「上次物理值 / 上次可见值」（`CLOCK_PHYS`/`CLOCK_VISIBLE`，全局唯一）。**差值小于除数时原样透出**（`Δ < 10` 就前进 `Δ`，不做整除），否则 `Δ/10` 会把紧挨着的两次读抹成同一个值，时钟看起来"冻住"，目标的小步长自增计时也会卡住。理由：开 VT 后 `cpuid`/`rdtsc` 要多绕一次 VM-exit，目标量到的差值天然大一个数量级，逐项测量（handler、转换、被抢占）永远有残差；按比例缩放**天然单调**（只做加法，不可能倒退，不需要钳制）、**误差同比缩小**（300 → 30），而且不依赖任何时间戳（MSR-store/陈旧/跨核迁移的问题全部绕开）。第一次读（`CLOCK_PHYS == 0`）直接返回物理值并建立基准；`physical <= CLOCK_PHYS` 同样按基准重来。代价是目标看到的是**慢 10 倍**的时钟，基于 `rdtsc` 的计时/超时会按比例变长——这是拿「测不出虚拟化」换「时钟不准」。这个模式下 `0x204` 的补偿不再影响返回值（账本仍算，仅供日志诊断）；
  - 命中时**每次都打一条日志、不限流**（日志**数值在前**，方便粘贴时不被截断：`TSC: hidden rdtsc since=.. root=.. guest=.. cpl=.. cs=.. pid=.. cpu#N by pid|module kthread=.. phys=.. -> .. host_time=.. exits=.. matched=.. overhead=.. rewind=.. stale=.. threads=.. rip=.. hits=..`）：要看清「到底哪一次 `RDTSC` 命中、CPL 多少」，抽样就会误导（内核侧命中比目标进程自己的命中多两个数量级，额度会先被内核吃光）。`kthread` 是账本的键（命中线程），`since` 是该线程「距离上一次隐藏读过了多少」，`host_time` 是该线程累计被扣掉的时间，`threads` 是已建账线程数，`exits`/`matched` 是两次读之间的退出总数与其中判成目标的次数，`rewind` 是**可见时钟被钳制（本会倒退）的次数**，`stale` 是时间戳判陈旧的次数，**`root` 是本次退出真正扣掉的 root 窗口（0 = 没扣，说明基准丢了）**，**`guest` 是本段 guest 运行了多久（命中退出会保留这段，突然变成几万 tick 说明里面混进了不产生退出的 guest 时间，比如 guest 自己的 ISR/DPC）**，`hits` 是累计命中数；
  - 没被隐藏的读取返回同一个物理计数器，**跨核天然一致**，不存在 0x5C 那类问题；
- **目标配置是全局的**（`selective_tsc` 的 `AtomicU32/AtomicU64`）：`RDTSC` 退出发生在 root，那里不能拿锁，所以用原子变量 + `CONFIG_GENERATION` 做无锁快照。每个 vCPU 在 `dispatch_vm_exit` 里发现自己是旧代时改自己的 `rdtsc_exiting`（`sync_tsc_hiding(cpu_id)`），不需要 IPI。**日志里的 cpu 号必须用 `vcpu.cpu_id`**：`VmxVcpu::vpid` 在所有核上都是 `GUEST_VPID`（=1），拿它打日志会显得「只有一个核在同步」；
- **匹配规则按特权级分工，两条路互不交叉**（`tsc::match_target`）：
  - `CPL3`：**只按进程，而且只从内核结构取**：`KPCR(IA32_KERNEL_GS_BASE) → [KPCR+0x188] KTHREAD → [+kthread_process] EPROCESS → [+eproc_pid] PID`。
    **翻译必须用 `system_cr3`，不能用当前 guest CR3**：开了 KVA shadow 的进程，用户态
    CR3 里内核半区没有映射，用它读 `[KPCR+0x188]` 直接失败（真机日志 `chain=None
    kthread=0x0`，而同一次读到的 `kpcr` 是好的）。KPCR/KTHREAD/EPROCESS 都是内核对象，
    在每个进程的系统 CR3 里都有映射。
    **不要再用 TEB 兜底**（`[gs + 0x40]` 那条已经删除）：用户态内存不受我们控制，
    真机上同一个进程先后出现过 `0xac256ab000` / `0xac256ad000` 两个不同的 "TEB"。
    代价是偏移表必须和本机 `ntoskrnl` 对得上（`0x203` 下发，`build == 0` 时 CPL3
    判定全部为「不匹配」）。失配时按 **PID 去重**打一行
    `TSC: miss pid=.. tgt=.. kpcr=.. c3=.. u3=.. kt=.. ep=..`（`c3` 系统 CR3、`u3` 用户
    CR3）。**字段用 `{:x}` 不用 `{:#x}`**：一条日志只有 152 字节，四个 `0x` 前缀会把
    最后那个 `eprocess` 截断成 `0xffff`；
  - `CPL0`：**只按内核模块**。`RIP ∈ [base, base+size)` 才算命中，不读进程号；账本的键是
    `current_kthread(0)`（`gs` 就是 KPCR）。`KPCR_CURRENT_THREAD = 0x188` 从 Win7 到 24H2
    没变，`tools/gen_win_offsets.py` 解析 `PsGetCurrentProcess` 时会核对它没漂；
  - 只用 guest 页表翻译，不碰宿主结构；读失败一律按「不匹配」处理，**绝不让一次读失败
    变成 guest 的 `#PF`**；
- **CPL 的取法**：`guest_cpl()` 取 `vmcs::guest::CsSelector` 的低 2 位。64 位模式下
  `CPL == CS.RPL`（Intel SDM Vol.3 §5.6），所以这里不需要看 access rights 的 DPL；
  CS 读失败（不该发生）退回 `SsSelector` 的 RPL（SS.RPL 也恒等于 CPL），两者都读不到
  才当 CPL0——**默认按 CPL0 而不是 CPL3**：CPL0 的链读失败只会漏报，拿 KPCR 当 TEB
  用却会读出垃圾 PID。日志里 `cpl=` 和 `cs=` 是并列的（CPL3 通常是 `0x33`、CPL0 是
  `0x10`），可以直接自证 CPL 没取错；
- **不再打 `pid miss` 日志**：`match_target()` 是**每次退出**都跑的（要给账本判 `matched`），所以「配了 PID 但当前不是目标」是常态——内核侧（DPC/ISR 在被打断线程的上下文里读 TSC）比目标进程自己的 `rdtsc` 多两个数量级，一旦按失配打日志就只剩噪声。要看失配只能看命中日志的 `cpl=`/`cs=`/`pid=`/`rip=`，或者临时加一次性打印；
- **偏移表是 CPL3 匹配的唯一依据**：需要 `kthread_process`/`eproc_pid`；`KernelArgs v4` 的 `win_offsets` 和 hypercall `0x203` 下发/覆盖它。**`build == 0` 时 CPL3 判定全部失效**（没有 TEB 退路可回），所以 uapp 必须先按 build 号选表再 `Start`；
- **性能代价**：只要有隐藏目标，全系统 `RDTSC` 都会 exit。忙等型代码（`while (rdtsc() - t0 < N)`）会明显变慢——这是「按进程选择性拦截」绕不开的成本，不用时用 `0x200 pid=0` / `0x202 size=0` 清掉目标，`rdtsc_exiting` 会自动关回去；
- 进程名查 PID（`0x201`）需要遍历 `PsActiveProcessHead`，链表头地址不在 `KernelArgs` 里，**当前返回 `-1`**，调用方用 `0x200` 直接传 PID；
- `IA32_TSC_AUX`、`RDMSR IA32_TSC`、HPET/PM Timer 都不在本机制范围内：它们读到的是**未调整**的物理值；
- **noumen 初始化必须跑在自己的栈上**：`_init` 只是 `init_stack.S::run_on_init_stack` 的薄壳，切到 64 KiB 静态栈再调 `init_body`，返回前把 rsp 还给 loader。loader 线程的内核栈只有十几 KiB，`setup` 链（VMCS 配置 + EPT 建表 + `_klog_fmt` 的 0x130 帧 + fmt 临时量）会把它用到守卫页，症状是 **0x7F / Arg1=8（DOUBLE_FAULT）**，dump 里 RSP 停在 `StackLimit` 上方几百字节、`KiInterruptDispatch` 的 TrapFrame 落在未映射页。

## 10. SVM 关键点与坑

> **TSC 虚拟化：SVM 暂不实现。** 拦截位（`Intercepts.rdtsc`/`rdtscp`）、`ControlArea.tsc_offset`
> 都保持不置位/为 0，guest 读到的就是物理 TSC；hypercall `0x200..0x206` 返回 `RAX = 3`。
> §9.8 那套只对 VMX 生效。

### 10.1 VMCB / 启动顺序

- `EFER.SVME` 必须在 `VMRUN` 前置位；失败会 `#UD`；
- `HSAVE` 区域必须 4 KiB 对齐，且在 VMCB 之前初始化；
- `VMCB` 的 clean bits 只能置位“未修改”的字段；改字段后要清对应 clean bit；
- `ASID` 每个 vCPU 唯一；不要复用正在运行的 ASID；
- `nCR3` 必须指向当前 vCPU 的 NPT 根；
- `GIF`/`STGI`/`CLGI` 顺序错误会导致中断丢失或 host 卡死；devirt 时必须先 `STGI` 再清 `EFER.SVME`；
- MSRPM 位语义是 **1 = 拦截，0 = 放行**；位号写错会表现为 guest 频繁 `#VMEXIT` 或 MSR 读写直接穿透；
- SVM 没有 VMX 的 `skip_instruction`；`VMMCALL` 必须由 hypercall dispatch 自己 `advance_rip`；
- `NRIPS` 可用时写 `next_rip`；不可用时只能先改 `save.rip`。

### 10.2 NPT

- identity NPT 覆盖 `min(2^MAXPHYADDR, 1 TiB)`（`CPUID.8000_0001H:EDX[26]` 支持 1 GiB 页时用 1 GiB 叶），否则 2 MiB 叶覆盖 `min(2^MAXPHYADDR, 64 GiB)`；PML4 1 页 + PDPT 1 页 + PD 表按需（≤64 张），每 vCPU 约 3–66 页。NPT 叶没有内存类型字段，类型由 guest PAT + HPA 的 MTRR 决定；
- **SVM 的 CET 由 VMCB 保存区自动处理**：`save.s_cet`/`save.ssp`/`save.isst_addr` 在 VMRUN/#VMEXIT 时随 guest 状态换入换出。`capture_cet_state` 在首次 VMRUN 前把当前值写进 VMCB（否则 VMRUN 会把 guest 的 CET 状态清零），并把 root 的 `IA32_S_CET` 置 0，避免 root 的 `ret` 消费 guest 影子栈；
- 默认 2 MiB 大页；hook 页按需 split 到 4 KiB；
- **NPT 没有 execute-only**：`NX` 只控制执行，读/写/执行不能像 EPT 那样分离；
- 因此 NPT hook 用三种 leaf 视图：

```text
OriginalExec  : 原始页, NX=0，执行
ShadowNx      : 私有副本, NX=1，数据读/写
ShadowExec    : 私有副本, NX=0，执行带补丁
```

- 命中 hook 后先把 leaf 切到 `ShadowNx`（数据视图），需要执行时再切 `ShadowExec`；
- 每次 VM exit 开头根据 `npt_shadow_view` 恢复 `ShadowNx`，避免 guest 读到补丁；
- 切换 leaf 后必须失效 NPT TLB（`INVLPGA`/`TLB_CONTROL`）；HideSelf 一次改完所有页后只发一次 `TLB_CONTROL`（`apply()` 末尾统一 flush）；
- 没有 execute-only，意味着数据读窗口和执行窗口本质上是时间片切换；这是 NPT hook 的固有弱点，不要宣称“完全不可见”。
- **当前 NPT hook 是非完整实现**：只有 byte-patch/function hook；没有 NPT 原生断点，`UserBreakpoint`（kind 4）返回 `Unsupported`，等待表没有对应的安装路径。

### 10.3 NPT function hook

- 执行副本入口写 `0xCC`，读/写副本保持原样；
- trap 后用 trampoline 保存 15 个 GPR，调用 handler，执行 stolen 指令，再跳回 `va + stolen`；
- trampoline 栈布局必须和 handler 的 `GuestContext` 完全一致；
- `STOP` 前恢复所有真实页字节，否则 `EFER.SVME=0` 后 `#BP` 无人处理，guest 会 `0x3B`；
- 单步 `#DB`、外部中断、NPF 都可能打断 hook 窗口；必须 abort/重新武装；
- `INVLPGA` 只失效翻译，不清理已解码指令；需要 self-modifying-code 触发（同值写 + `CLFLUSH` + `MFENCE`）才能让热代码页重新取指。

### 10.4 SVM hypercall

- key 错误：注入 `#UD`，返回 `false`；
- 未知命令：`RAX = 3`；
- `Stop`：推进 RIP → `devirtualize`，不返回；
- SVM 专属 `0x30..0x3a` 在 SVM 分发；VMX 对它们返回 `3`；
- 不要把 SVM 的 `advance_rip` 移到共享层。

### 10.5 退出桩与状态保存

- 退出时先 `VMSAVE guest`，再 `VMLOAD host`；两者都操作当前 CPU 的段寄存器，顺序反了会把 guest 的 FS/GS 留在 host；
- 主机 HSAVE 页必须在启动时先用 `VMSAVE` 初始化；否则第一次 `VMLOAD` 会把全 0 装进 FS/GS，`Vcpu::current()` 立刻失效；
- 返回 guest 的路径是 `VMLOAD guest` → `VMRUN`；退出时已经 `VMSAVE` 过，不要重复；
- `TLB_CONTROL` 在**下一次 `VMRUN`** 生效，不是写完立即生效；不要在每个 exit 都写 `ALL`，否则会把“按需失效”退化成永久全刷；
- ASID 0 是 host 保留值；guest vCPU 用非 0 ASID，且不要复用正在运行的 ASID。

### 10.6 事件注入与 MSRPM

- SVM 没有 VMX 的“软件异常”注入类型；`#BP`/`#OF`/`#UD` 等软件异常必须用 exception 类型注入，并保证 `next_rip`/指令长度正确，否则 guest 会重复执行同一条指令；
- NMI 计数递减必须防下溢；下溢成 `u64::MAX` 会无限注入 NMI；
- MSRPM 是 4 个 2 KiB 区间、每 MSR 2 bit；不要按 VMX 的 4×1 KiB 布局写；
- `0xC0010000` 区间必须覆盖；区间外 MSR 默认拦截时，guest 读不存在的 MSR 会收到我们注入的 `#GP`，而不是硬件 `#GP`；
- `VM_CR.SVMDIS` 必须为 0；`CPUID.8000_0001H:ECX[2]` 必须为 1；
- `CPUID.8000_000AH` 要按 SVM 能力伪造；guest 执行 `VMRUN` 时应注入 `#UD`（与 CPUID 一致）。

## 11. EPT 原生断点等待协议

> **术语**：VMX 的 `UserBreakpoint` / `user_breakpoint` 在文档中称为 **EPT 原生断点**。它通过 EPT 视图和 `INT3` 在用户 VA 命中，等待调用方通过 hypercall 应答，不是硬件 DR 断点。
>
> SVM 没有实现 NPT 原生断点：`UserBreakpoint`（kind 4）返回 `Unsupported`，`npt/hook/wait` 的等待表没有对应的断点生产/安装路径。

- 命令 `13`：只读快照，不消费等待项；
- `14`：ACK，`seq` 必须匹配；动作 `Continue/SingleStep/...`；
- `15`：设置超时，`-1` 无限，`0` 默认，>60000 截断；
- `16`：修改等待项寄存器；
- `17`：线程过滤（`ETHREAD` 或 0）；
- `18`：取消等待项；
- `19`：一次性断点（命中后禁用）。

约束：

- 等待项在 hook 命中时建立，线程可能迁移；单步断点不能只放在 per-vCPU 状态；
- `#DB`/MTF 被其他 exit 打断时，one-shot 必须重新武装，因为原指令没有执行；
- ACK 后写回寄存器再继续；
- 断点字节必须在所有核切换 clean 视图后提交（`21`），不能提前写。

## 12. 内存服务 `MEM_*`

- `0x20 MEM_QUERY`：返回 ABI 版本、批量上限、最大字节、直接映射大小；
- `0x21 MEM_BATCH`：guest 提供请求环，批量读/写/翻译；`R9 bit0` 为 dry-run；
- `0x22 MEM_SCAN`：分段扫描 guest VA；
- `0x23 MEM_MAP`：输出物理区间 `(start,size)`；
- `0x24 MEM_STATS`：每 CPU 统计快照。

规则：

- VMX 使用 EPT 视图；SVM 使用 identity NPT + 直接映射；
- 所有入口先检查 `available()`；不可用返回 `RAX = Unsupported`；
- 读/写 guest 用 `copy_to_guest`/`copy_from_guest`，不要直接解引用 guest VA；
- CR3 低 12 位是 PCID，翻译前掩掉；
- `MEM_BATCH` 失败条目返回 `MemError`，不要把整个环丢掉；
- `MEM_SCAN` 的请求结构按值读入、按值写回，注意并发/TOCTOU；
- `MemMap` 的区间表来自 `KernelArgs.phys_map_base`，数组以全 0 项结尾；
- 统计按 CPU 分槽，槽数取在线 CPU 数。

## 13. 日志与 panic

- `klog!`：普通日志；没有 sink 时编译为死分支，参数仍参与类型检查；
- `klog_verbose!`：只在 `debug_assertions` 且存在 sink 时输出；release 默认丢弃；
- `klog_try!`：非阻塞，host exception/panic 路径使用；
- 日志环有两种来源：loader 分配并通过 `KernelArgs.log_base/log_size` 传入；或 `log::init(0, _)` 让 hypervisor 从页池分配内部环（`LOGGER_BUFFER_SIZE` = 77872 字节，占 20 页），此时 `log_size` 被忽略，guest 用 `FLUSH_LOGS`(1) 取走记录；
- 内部环要求页池已初始化：`main` 先 `memory::init()`，再 `log::init()`；页池不可用时日志保持关闭，不 panic；
- 环是 512 条、每条 152 字节；满后覆盖最旧记录；
- 读取方用 seqlock：读 `sequence`（奇数表示写入中）→ 复制 → 再读，变化则重试；
- panic 报告写到所有已编译 sink；`panic-detail` 不隐含串口；
- 不要在 panic 路径分配内存或拿阻塞锁。

## 14. 构建/工具链坑

1. **`rust-lld` 缺失**：当前 nightly 不自带 `rust-lld`，`.cargo/config.toml` 使用绝对路径 `C:\Program Files\LLVM\bin\ld.lld.exe`。换机器要改路径。
2. **clang 临时目录**：`build.rs` 把 `TMP/TEMP` 指到 `OUT_DIR/cc-tmp`；否则某些 Windows ACL 配置下 clang 会失败。
3. **target 目录**：每个 crate 用自己的 `target/`（`hypervisor/`、`um/uapp/`、`um/nsvr/`、`tools/rdtsc_probe/`），仓库根 `target/` 只放暂存产物。不要恢复 `.cargo-target`，也不要把 `target-dir` 指到别的 crate。如果 `target` 被其他进程锁定，先关掉占用进程，不要换目录绕过。
4. **SVM 宿主测试的 cargo 配置**：`tests/test-svm-core.py` 默认在仓库外临时目录编译；不要把测试 crate 放到仓库 `target/` 下，否则会继承 `.cargo/config.toml` 的 `x86_64-unknown-none` + `build-std`，出现 `panic_handler` 缺失等假错误。
5. **feature 组合**：每次改 `cfg` 后至少跑默认、release、`vmx-ept` only、`svm-npt` only、无 feature 五种构建。

## 15. 测试

```powershell
# EPT hook 纯逻辑
python tests/test-ept-core.py

# SVM 纯逻辑
python tests/test-svm-core.py

# mem 纯逻辑
python tests/test-mem-core.py
```

测试脚本在 `tests/`；`tests/svm-core-tests` 是独立的宿主 harness crate，不能直接对主 crate 执行 `cargo test`。

测试边界：

- 宿主测试不覆盖真实 VMX/SVM 硬件路径；
- EPT 测试不覆盖 EPT violation、`#BP`、MTF；
- SVM 测试不覆盖 VMRUN、NPT TLB、真实 NPF；
- 硬件路径必须按本文件第 19/20 节上机验证。

## 16. 调试与验证

### 日志

- 默认只有 logbuffer；显式开启 `serial-console` 才有 COM1；
- no-serial 镜像用 logbuffer，loader 周期线程落盘（默认 `C:\noumen.log`）；
- `log_base == 0` 的内部环不在 loader 手里，驱动不会落盘；调用方用 `FLUSH_LOGS`(1) 取记录；
- 崩溃转储搜索 `noumen.hv.log`，按 header 的 `msg_start/msg_count` 解析；
- `id` 断号表示记录被覆盖。

### 常见失败

| 现象 | 优先检查 |
| --- | --- |
| 调用 hypercall 直接 `#UD` | key 是否为 0/错误；该核是否已虚拟化；feature 是否关闭 |
| guest 蓝屏 `0x3B` | STOP 前 hook 字节是否恢复；`EFER.SVME`/VMXOFF 顺序 |
| 单核正常、多核蓝屏 | hook generation/`HOOK_APPLY`/`HOOK_RETIRED`；每核 EPT/NPT 是否同步 |
| 读 guest 内存失败 | CR3 是否当前 guest、PCID 是否掩掉、直接映射是否可用 |
| NPT hook 偶发不命中 | 视图切换/TLB 失效/热代码页未重新取指 |
| EPT 原生断点卡住 | ACK `seq`、超时、线程过滤、单步是否被其他 exit 打断 |

## 17. 文档状态

仓库长期文档：

- `README.md`：中文项目概览、feature、构建、日志；
- `README_en.md`：英文项目概览；
- `AGENTS.md`：架构、ABI、全部坑、EPT/SVM/mem 细节。

`docs/` 已移除，内容已合并到本文件。`tests/` 保留宿主测试脚本和 SVM harness；不要重新引入外部项目分析、旧移植计划或驱动/调用方专项文档。

## 18. 修改检查清单

改代码前：

- [ ] 确认属于哪个后端；共享层是否被污染；
- [ ] 确认 feature 关闭时的行为（返回 `3`、不编译、或 `#UD`）；
- [ ] 确认是否需要所有核同步；
- [ ] 确认 STOP/devirt 时能恢复。

改代码后：

- [ ] `cargo build`
- [ ] `cargo build --release`
- [ ] `cargo build --no-default-features --features vmx-ept`
- [ ] `cargo build --no-default-features --features svm-npt`
- [ ] `cargo build --no-default-features`
- [ ] `python tests/test-ept-core.py`
- [ ] `python tests/test-svm-core.py`
- [ ] `python tests/test-mem-core.py`
- [ ] 更新本文件/README 的路径、编号和语义

## 19. EPT hook 细节

### 19.1 Hook 类型与错误码

| 值 | `HookKind` | 用途 |
| --- | --- | --- |
| 0 | `Breakpoint` | 内核/用户 VA 的 `0xCC` 断点，恢复原页 + MTF 单步 |
| 1 | `DetourInternal` | hypervisor 内部 trampoline detour |
| 2 | `Hide` | 数据读零页；取指切回真实页（与 HideSelf 的“全零页”不同） |
| 3 | `DetourTrampoline` | 调用方提供 proxy/origin 的 detour（仅内核 VA） |
| 4 | `UserBreakpoint` | EPT 原生断点（VMX）：仅用户 VA，`INT3` 自旋等待调用方应答；SVM 返回 `Unsupported` |

`HookError`（`RAX` 返回，不注入异常）：

```text
1 InvalidAddress   2 NotMapped      3 Unsupported    4 NoFreeSlot
5 NoMemory         6 AlreadyExists  7 NotFound       8 HypercallsDisabled
9 WrongPageKind    10 UnsupportedCpu
```

限制：

- 最多 32 个槽位；同页最多 8 个普通/EPT 原生断点；每页最多 1 个 detour/hide；
- 跨页 detour 占一个槽位、两个物理页；
- EPT 原生断点只接受低半区 VA，命中要求 CPL3，否则退回普通 MTF；
- 事件环 256 条，满了丢最旧；非 owner 核跳过的断点不产生事件。

### 19.2 EPT 页状态

| 状态 | R | W | X | 指向 | 用途 |
| --- | --- | --- | --- | --- | --- |
| `RealDataOnly` | 1 | 1 | 0 | 真实页 | 静止：数据原生通过，取指陷入 |
| `CopyExecOnly` | 0 | 0 | 1 | 私有副本 | 执行含 `0xCC`/跳转的副本 |
| `CopyDataOnly` | 1 | 1 | 0 | clean 副本 | 字节补丁的数据视图 |
| `RealPassthrough` | 1 | 1 | 1 | 真实页 | 已移除或 MTF 临时放行 |

转换：

```text
RealDataOnly --取指 violation--> CopyExecOnly
CopyExecOnly --数据 violation--> RealDataOnly（不推进 RIP，重试）
```

要点：

- fake/origin 都是 execute-only，数据访问必须切回真实页；
- 入口用 `mov r11, imm64; jmp r11`，不要用 `jmp [rip+0]`；
- `stolen_len` 覆盖完整指令，跨页拒绝；
- 非 MTF exit 打断单步窗口时，abort 单步并重新武装 one-shot；
- `HOOK_APPLY` 只作用当前核，所有核 retire 后才能释放旧 origin。

### 19.3 HideSelf

- 隐藏整个镜像分配：PT_LOAD、`.noumen.pool`、`.noumen.pfns`、`.symtab/.strtab`、尾部 PFN 表；
- VMX 把 guest 读/写/取指都导向本核 dummy 零页；SVM 指向 `ShadowNx` 零页（NX），取指触发 NPT 异常；
- host 仍可通过宿主页表读真实页；
- dummy/零页每核一份，guest 写它不影响真实镜像；
- HideSelf 的代价和镜像大小成正比：页池 32 MiB 时镜像约 8.4 千页，每页一次 EPT 叶改写；批量窗口内只发一次 `INVEPT`，每核首次 `sync` 的停顿在毫秒级；
- Stop 只停虚拟化，不释放镜像；镜像 PFN 表随镜像卸载释放。

## 20. SVM 上机验收

前置：

- AMD CPU，BIOS 未禁用 SVM，`CPUID.8000_0001H:ECX[2]=1`；
- 测试签名开启；串口或 `buffer-log` 可用；
- 构建含 `svm-npt,mem`（例如 `--no-default-features --features panic-detail,buffer-log,svm-npt,mem`）。

检查顺序：

| 项 | 期望 | 失败先看 |
| --- | --- | --- |
| SVM 特性 | 不返回 `UnsupportedCpu` | `VM_CR.SVMDIS`、BIOS |
| `PING(3)` | `RAX = PONG ^ RDX` | key、VMMCALL 拦截 |
| `CPUID.8000_0001H:ECX[2]` | guest 读到 0 | CPUID 过滤 |
| `CPUID.8000_000AH` | 按能力伪造 | CPUID 过滤 |
| EFER RDMSR | 无 SVME 位 | MSRPM 0xC0000080 |
| `VM_CR`/`VM_HSAVE_PA` | 0 | MSRPM 位号 |
| 首次 VMRUN | 不出现 `exit_code=0xFFFF_FFFF` | VMCB 保留位/clean bits、HSAVE |
| 正常退出 | 只有 CPUID/MSR/VMMCALL/NPF/异常 | 拦截位是否多开 |
| `STOP(4)` | 当前核 SVME=0，guest 继续 | devirt 顺序：先 STGI 再清 SVME |
| 其它核 | 仍虚拟化直到各自 STOP | 每核独立 |
| NPT 常规访问 | 无 NPF | identity 表、ncr3、NP_ENABLE |
| 超出覆盖（1 GiB 模式 >1 TiB，2 MiB 模式 >64 GiB） | 停机并打印 NPF/EPT violation | 预期限制 |
| hook 页 | 2 MiB 拆成 4 KiB | split_pde、页池 |
| `HIDE_SELF(9)` | 返回隐藏页数 | kernel_pfn_* |
| `READ_SELF(10)` | 读到真实镜像 | 直接映射范围 |
| `READ_SELF_EPT(11)` | 隐藏后读到 0 | leaf pfn、ShadowNx |
| `MEM_QUERY` | `RAX=1,RDX=126,R8=516096` | 常量 |
| `MEM_BATCH` | 条目 status 正确、载荷写入环 | CR3、翻译 |
| `MEM_MAP` | 物理区间 | range_total |
| `MEM_SCAN` | 分段 cursor/done/status 正确 | 200 B 请求布局 |
| 多核 hook | 迁移后仍命中，事件 cpu 正确 | generation、sync |
| 每核 ASID | 不同且非 0 | setup 的 NASID 检查 |

> SVM 没有 NPT 原生断点，也没有对应的断点等待安装路径；`WAIT_*` 和断点单步不列入 SVM 验收，相关内容只适用于 VMX 的 EPT 原生断点。

常见失败：

| 症状 | 原因 |
| --- | --- |
| 第一次正常，之后 FS/GS/STAR 旧值 | 重入前漏了 `VMLOAD guest` |
| setup 阶段 0x1E/#GP，RIP 在 rdmsr | 读了 Intel-only MSR（如 0x1A0） |
| 日志刷 `code=0x7c`、系统极慢 | MSRPM 位值反了（1=拦截） |
| `Vcpu::current()` 错 | 宿主 FS base 没指向 Vcpu，或 VMLOAD host 漏了 |
| 宿主访问自身内存缺页 | 没切 host CR3 |
| NMI 在 host 乱入 | root 执行了 STGI（应保持 GIF=0） |
| 移除后其它核 NPF | sync 未生效或 leaf 恢复失败 |
| STOP 后 guest 收不到中断 | devirt 顺序错：STGI 必须在清 SVME 之前 |

## 21. `MEM_*` ABI 细节

### 21.1 常量

```text
RING_MAGIC      0x4D45_4D4E ("NMEM")
RING_ABI        1
MAX_BATCH_OPS   126
DATA_OFFSET     0x1000
MAX_OP_BYTES    0x1000
MIN_RING_BYTES  64 KiB
MAX_RING_BYTES  4 MiB
MAX_BATCH_BYTES 126 * 4 KiB = 516096
DIRECT_MAP_SIZE 512 GiB
```

`DIRECT_MAP_SIZE` 是**宿主**直接映射的窗口，仍是 512 GiB（占满一个 PML4 槽，`DIRECT_PML4_INDEX = 255`）。**guest 的 EPT 恒等映射是另一套**，粒度与覆盖在 `check_vmx_features()` 里决定：

| 条件 | 页粒度 | 覆盖 |
| --- | --- | --- |
| 支持 `IA32_VMX_EPT_VPID_CAP.PDPTE_1GB` | 1 GiB | `min(1 << MAXPHYADDR, 1 TiB)` |
| 不支持 | 2 MiB | `min(1 << MAXPHYADDR, 64 GiB)` |

PDPT/PD 表按实际用到的段按需分配；`hide_self` 等批量改表路径用 `begin_batch()`/`end_batch()` 把 INVEPT 合并成一次。hook/HideSelf 需要的 4 KiB 视图按 1 GiB → 2 MiB → 4 KiB 逐级拆。

**1 GiB 叶的 MTRR 约束**：EPT 叶自带内存类型，一个 1 GiB 叶只能写一个类型，而 MTRR 区间比 1 GiB 细（fixed MTRR、PCI 空洞、变量区间边界）。`prepare_1gb` 用 `MtrrData::is_uniform_1gb()`（只比较区间边界，不采样）判断：段内无 MTRR 边界 → 1 GiB 叶；否则分配 1 张 PD 表拆成 512 个 2 MiB 叶，各自取 `memory_type()`。混合段数量的日志只打一行汇总。

**SVM 的 NPT 是同一套覆盖策略**，但叶子没有内存类型字段：`CPUID.8000_0001H:EDX[26]`（Page1GB）为 1 时用 1 GiB 叶覆盖 `min(2^MAXPHYADDR, 1 TiB)`，否则用 2 MiB 叶覆盖 `min(2^MAXPHYADDR, 64 GiB)`；PDPT 1 页 + PD 表按需分配（≤64）。内存类型由 guest PAT + HPA 的 MTRR 决定，和裸机一致，不需要（也无法）在 NPT 叶里写。

### 21.2 环

`MemRingHeader` 64 字节：

```text
magic u32 | abi u16 | flags u16 | capacity u32
entry_capacity u16 | head u16 | tail u16 | reserved0 u16
seq u32 | error u32 | reserved1 u32
batches u64 | ops u64 | bytes u64 | root_tsc u64
```

`MemOp` 32 字节：

```text
op u16 | status u16 | len u32 | cr3 u64
addr u64 | off u32 | aux u32
```

操作码：

```text
0 READ_VIRT   1 WRITE_VIRT   2 READ_PHYS
3 WRITE_PHYS  4 TRANSLATE
```

规则：

- 单生产者/单消费者，无锁；同一时刻只允许一个 VMCALL 在飞；
- 环在调用方用户内存，hypervisor 用当前 CR3 翻译后做 GPA→GPA 拷贝；
- 不需要 MDL/PFN 数组/会话状态；
- `flags` 必须 0；`entry_capacity ≤ 126`；
- 头错误：1 = 头非法，2 = 下标非法，3 = 环不可访问；
- `dry_run` 只校验，不执行、不写载荷。

### 21.3 `MEM_SCAN`

`MemScanRequest` 固定 200 字节：

```text
abi u16 | space u8 | flags u8 | status u16 | pattern_len u16
max_matches u16 | matches u16 | done u8 ...
```

- 请求按值读入、按值写回；
- 游标由调用方持有；每次调用保证至少前进一页；
- 并发扫描第二个调用返回 `Busy`；
- 不访问不可用页时按 `SKIP_INACCESSIBLE`/`NotPresent` 处理。

### 21.4 `MemError`

```text
0 Ok          1 NotPresent   2 NonCanonical  3 OutOfMap
4 BadCr3      5 NotRam       6 TooLarge      7 BadEntry
8 Busy        9 HostFault    10 Unsupported  11 Hooked
12 Unaligned
```

`MEM_*` 失败通过 `RAX` 或条目 `status` 返回，不注入异常；写被 hook 的页返回 `Hooked`。
