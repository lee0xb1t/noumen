# TSC 选择性隐藏设计文档

## 1. 概述

实现针对特定进程/内核模块的 TSC（时间戳计数器）虚拟化，隐藏 VM-exit 开销。采用选择性隐藏策略：只对目标进程/模块的 RDTSC 返回调整后的值，其他进程返回物理 TSC。

## 2. 设计目标

- 隐藏特定进程的 VM-exit 开销（游戏反作弊、性能测试等）
- 保持其他进程透明，避免多核同步问题
- 支持用户模式（CPL=3）和内核模式（CPL=0）进程
- 通过 hypercall 动态配置目标进程/模块

## 3. 核心机制

### 3.1 多层次检查逻辑

```
RDTSC 执行
    ↓
检查 selective_tsc.enabled
    ├─ false → 返回物理 TSC（无 exit）
    └─ true → 继续检查
        ↓
    读取当前进程 ID（从 ETHREAD/EPROCESS）
        ↓
    CPL == 3（用户模式）
    ├─ current_pid == target_pid → 返回隐藏值（offset -= host_overhead）
    └─ 否则 → 返回物理 TSC
    
    CPL == 0（内核模式）
    ├─ current_pid == target_pid → 返回隐藏值
    ├─ RIP in target_module → 返回隐藏值
    └─ 否则 → 返回物理 TSC
```

### 3.2 隐藏方式

**当前实现**：禁用硬件 TSC offset（原因：per-vCPU offset 导致多核不一致 → 0x5C BSOD）

**选择性隐藏路径**：
1. 启用 `RDTSC_EXITING`（拦截 RDTSC 指令）
2. 在 exit handler 中判断是否目标进程
3. 若是：返回 `物理_TSC - 累积_host_开销`
4. 若否：返回物理 TSC（无修改）

**优点**：
- 只对目标进程隐藏，无多核同步问题
- 其他进程保持高性能（无 exit）
- 灵活选择隐藏对象

## 4. 架构设计

### 4.1 Hypervisor 侧（Rust）

#### 4.1.1 新增结构（`src/kargs.rs`）

```rust
#[repr(C, packed(4))]
pub struct WinOffsets {
    pub build: u32,           // Windows build 号
    pub eproc_pid: u32,       // EPROCESS.UniqueProcessId 偏移
    pub eproc_name: u32,      // EPROCESS.ImageFileName 偏移
    // ... 其他偏移字段
}

// KernelArgs v4 包含
pub struct KernelArgs {
    // ... 现有字段
    pub win_offsets: WinOffsets,  // 新增
}
```

#### 4.1.2 选择性隐藏配置（`src/arch/x86_64/vmx/state/selective_tsc.rs`）

```rust
pub struct SelectiveTscConfig {
    pub enabled: bool,                    // 是否启用隐藏
    pub target_pid: ProcessId,            // 目标进程 ID
    pub target_module: Option<KernelModule>,  // 目标内核模块
    pub accumulated_overhead: u64,        // 累积 host 开销
}

pub struct ProcessId(pub u32);

pub struct KernelModule {
    pub base: u64,    // 模块基址
    pub size: u64,    // 模块大小
}

impl SelectiveTscConfig {
    pub fn set_target_pid(&mut self, pid: u32);
    pub fn set_target_module(&mut self, base: u64, size: u64);
    pub fn should_hide_tsc(&self, cpl: u8, rip: u64, current_pid: ProcessId) -> bool;
}
```

#### 4.1.3 Hypercall 接口（`src/arch/x86_64/hypercall.rs`）

```rust
pub enum Hypercall {
    // ... 现有
    TscSetTargetPid = 0x200,      // 设置目标 PID + 模式
    TscSetTargetName = 0x201,     // 通过名称查询 PID
    TscSetTargetModule = 0x202,   // 设置目标模块
    TscSetWinOffsets = 0x203,     // 运行时下发 WinOffsets
    TscSetExitOverhead = 0x204,   // 每次命中退出的补偿 tick
}
```

**参数约定**：

| Hypercall | RDX | R8 | R9 | RAX (返回) |
|-----------|-----|-----|-----|-----------|
| 0x200 | PID | 模式（0 内核 / 1 用户 / 2 都算） | - | 0 或错误码 |
| 0x201 | 名称 VA | 名称字节数 | - | PID 或 -1 |
| 0x202 | 模块名 VA | 基址 | 大小 | 0 或错误码 |
| 0x203 | `WinOffsets` VA | 字节数（>= 56） | - | 0 或错误码 |
| 0x204 | 补偿 tick | - | - | 0 或错误码（1 = 超范围） |

#### 4.1.4 VMX Hypercall Handler（`src/arch/x86_64/vmx/hypercall/tsc.rs`）

```rust
pub fn set_target_pid(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()>;
pub fn set_target_name(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()>;
pub fn set_target_module(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()>;
pub fn set_win_offsets(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()>;
pub fn set_exit_overhead(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()>;
```

### 4.2 Loader 侧（C#/Rust）

#### 4.2.1 初始化流程

```
加载 noumen
    ↓
查询 Windows build 号
    ↓
从 win_offsets.rs 查询偏移表（memflow-win32 格式）
    ↓
填充 KernelArgs.win_offsets
    ↓
调用 noumen 入口
```

#### 4.2.2 UI 选项卡（`loader/windows/deviceio/uapp/src/tabs/tsc.rs`）

新增 TSC 隐藏配置选项卡：

- **进程选择**：
  - 进程名输入框（如 "game.exe"）
  - "查询 PID" 按钮 → 调用 hypercall 0x201
  - 或直接输入 PID → 调用 hypercall 0x200

- **模块选择**：
  - 模块名输入框（如 "game.dll"）
  - 基址和大小输入
  - "设置模块" 按钮 → 调用 hypercall 0x202

- **状态显示**：
  - 当前目标进程
  - 隐藏状态（启用/禁用）

## 5. 实现路线图

### 第 1 阶段：Hypervisor 核心（已完成）

- [x] 扩展 KernelArgs 添加 WinOffsets (v3→v4，112→168 字节)
- [x] 添加 hypercall 命令定义（0x200..0x204）
- [x] `selective_tsc` 模块：全局目标配置（无锁原子量）+ 每 vCPU 时间账本
- [x] 在 VMX hypercall dispatch 集成命令
- [x] 实现 RDTSC/RDTSCP 拦截 handler（`emulate_rdtsc`/`emulate_rdtscp`）
- [x] 实现进程检查逻辑（GS→KTHREAD→EPROCESS→PID，走 guest 页表翻译）
- [x] `rdtsc_exiting` 跟随配置代数懒同步（无 IPI）

### 第 2 阶段：Loader 集成（已完成）

- [x] 驱动新增 `NoumenCommandSetWinOffsets`(15)，`Start` 时合并进 `KernelArgs.WinOffsets`
- [x] uapp `win_offsets.rs` 增加 `kthread_process` 字段（19041 用 `dt` 核对：0x98+0x20=0xB8）
- [x] uapp 在 `Start` 之前自动下发偏移表（按 build 选表，选不到就发 `build = 0`）
- [x] 新增 TSC 选项卡（目标 PID / 目标模块基址+大小 / 退出补偿 / 清除 / 状态）

### 第 3 阶段：验证与优化（进行中）

- [x] 编译测试所有 feature 组合（5 种配置 0 warning）
- [x] 宿主测试（EPT 63 / mem 6 / uapp 33）
- [x] 真机验证：CPL3 的 PID 判定走 TEB/`IA32_KERNEL_GS_BASE`（日志出现 `cpl=3` 命中）
- [x] 定位残差来源：账本测不到 guest→root 转换，`rdtsc; cpuid; rdtsc` 的差值 ≈ 两次转换之和
- [x] 补偿通道：hypercall `0x204` + TSC 面板“退出补偿”
- [ ] 实测残差并填补偿值，把目标进程量到的差值压到 0 附近
- [ ] 真机验证：清除目标后 `rdtsc_exiting` 关闭、性能恢复
- [ ] 性能基准：忙等循环的减速比

### 已实现的行为细节

| 项 | 当前实现 |
| --- | --- |
| 隐藏值 | `物理TSC − 该线程的 host_time`；账本**按线程分**（KTHREAD 指针为键，每 vCPU 8 个槽位），命中退出补 `exit_overhead` |
| CPL 来源 | `CsSelector & 3`（`CS.RPL == CPL`），读失败退回 `SsSelector & 3`，都不行按 CPL0 |
| CPL3 判定 | `*(gs+TEB.ClientId=0x40)` → PID，读不到再走 `IA32_KERNEL_GS_BASE` 的 KPCR 链 |
| CPL0 判定 | `当前 PID == 目标 PID` 或 `RIP ∈ 模块范围` |
| 进程号来源 | `*(gs+KPCR.CurrentThread=0x188)` → KTHREAD → `[+kthread_process]` → EPROCESS → `[+eproc_pid]` |
| 目标判定次数 | 每次退出只算一次（`dispatch_vm_exit`），`RDTSC` 处理复用 |
| 偏移表 | `KernelArgs.WinOffsets` 或 hypercall `0x203` 覆盖（`build == 0` = 未提供/清除） |
| 线程分账 | `note_exit` 把「上一个 handler 的 root 时间」记到 `last_thread` 账上；`hide` 只减命中线程的账，避免同进程多线程互相污染 |
| 诊断日志 | 命中**全部输出、不限流**：`cpl=`/`cs=`/`rip=`/`pid=`/`kthread=`/`since=`/`host_time=`/`threads=`/`overhead=`/`rewind=`/`stale=`/`hits=`；不再打 `pid miss` |
| 无目标时 | `rdtsc_exiting = 0`，RDTSC 原生，零额外开销 |
| 有目标时 | `rdtsc_exiting = 1`，全系统 RDTSC 都 exit，再按进程决定是否隐藏 |
| 进程名查 PID | **未实现**（`0x201` 返回 `-1`，需要 `PsActiveProcessHead`） |
| SVM | **未实现**（仅 VMX 路径） |


## 6. 未实现项详细说明

### 6.1 VMX Hypercall Dispatch 集成

**位置**：`src/arch/x86_64/vmx/hypercall/mod.rs` 第 99-102 行

**需要做**：在 `Hypercall::MemStats` 之后添加三个新命令的 match 分支

```rust
Hypercall::TscSetTargetPid => tsc::set_target_pid(context, vcpu),
Hypercall::TscSetTargetName => tsc::set_target_name(context, vcpu),
Hypercall::TscSetTargetModule => tsc::set_target_module(context, vcpu),
```

### 6.2 RDTSC 拦截 Handler

**位置**：`src/arch/x86_64/vmx/exit_handler/misc.rs`（或新增 tsc.rs）

**需要做**：
1. 拦截 `EXECUTE_RDTSC` exit reason
2. 读取当前进程 ID
3. 调用 `SelectiveTscConfig::should_hide_tsc()` 判断
4. 若隐藏：返回 `物理_TSC - accumulated_overhead`
5. 若不隐藏：返回物理 TSC 并 skip 指令

### 6.3 进程检查逻辑

**位置**：`src/arch/x86_64/vmx/state/selective_tsc.rs` 中的 `get_current_process_id()` 函数

**需要做**：
1. 读取 GS:0x188（KTHREAD 的 ApcState.Process）
2. 使用 `KernelArgs.win_offsets.eproc_pid` 读 EPROCESS 的 PID
3. 处理读取失败（返回 0）

### 6.4 Loader 传入 WinOffsets

**位置**：`loader/windows/deviceio/uapp/src/tabs/vmx.rs`（或 driver 层）

**需要做**：
1. 查询 Windows build 号（从 registry 或 GetVersionEx）
2. 调用 `win_offsets::select(build)` 获取偏移表
3. 填充 KernelArgs.win_offsets
4. 传递给 hypervisor

### 6.5 TSC 隐藏选项卡

**位置**：`loader/windows/deviceio/uapp/src/tabs/tsc.rs`（新建）

**需要做**：
1. 创建 UI 布局（进程输入、模块输入）
2. "设置进程名" 按钮 → 调用 hypercall 0x201
3. "设置 PID" 按钮 → 调用 hypercall 0x200
4. "设置模块" 按钮 → 调用 hypercall 0x202
5. 显示当前隐藏状态

### 6.6 TSC 隐藏功能实现

**位置**：`src/arch/x86_64/vmx/hypercall/tsc.rs`

**已有**：函数框架 + hypercall 定义

**需要做**：
1. `set_target_pid`：直接调用 `vcpu.selective_tsc.set_target_pid(pid)`
2. `set_target_name`：遍历 EPROCESS 链表查找进程名，返回 PID
3. `set_target_module`：调用 `vcpu.selective_tsc.set_target_module(base, size)`

## 7. 数据流示例

### 场景：隐藏 game.exe 的 RDTSC

```
Loader 启动：
  1. 查询 Windows build → 19041
  2. win_offsets::select(19041) → 获取偏移表
  3. KernelArgs.win_offsets = 偏移表
  4. 调用 noumen

Noumen 启动：
  5. kargs::KernelArgs 包含 win_offsets
  6. 每个 vCPU 初始化 SelectiveTscConfig (disabled)

Loader 配置目标进程：
  7. 调用 Hypercall 0x201: rdx="game.exe", r8=8
  8. Noumen 在 EPROCESS 链表中查找 "game.exe"
  9. 返回 PID (假设 1234)

Loader 启用隐藏：
  10. 调用 Hypercall 0x200: rdx=1234（PID），r8=2（内核+用户）
  11. Noumen: selective_tsc.set_target_pid(1234)
  12. selective_tsc.enabled = true

Guest 进程执行 RDTSC：
  13. CPU 触发 EXECUTE_RDTSC exit
  14. Handler 读取当前 PID
  15. 若 == 1234：返回 物理_TSC - accumulated_overhead
  16. 否则：返回物理 TSC

测试软件（game.exe）：
  17. rdtsc → cpuid → rdtsc
  18. 差值被隐藏（不包含 hypervisor 开销）
```

## 8. 构建配置

所有 feature 组合必须编译通过：

```bash
cargo build                           # 默认（包含 vpx-ept）
cargo build --release
cargo build --no-default-features --features vmx-ept
cargo build --no-default-features --features svm-npt
cargo build --no-default-features    # 无 EPT/NPT
```

## 9. 测试策略

### 9.1 单元测试

- ProcessFilter 匹配逻辑
- KernelModule 范围检查
- SelectiveTscConfig 状态转换

### 9.2 集成测试

- Hypercall 0x200/0x201/0x202 调用
- RDTSC 拦截与返回值
- 进程切换时隐藏状态保持

### 9.3 真机测试

- game.exe 性能测试工具检测
- 反作弊驱动检测
- 多核 TSC 一致性

## 10. 已知限制

1. **只支持 Windows**：需要 EPROCESS 结构和 PID 概念
2. **Linux 不支持**：无 PID 的等价概念
3. **SVM 暂不支持**：当前实现仅 VMX，SVM hypercall 返回"不支持"
4. **进程名查询**：需要遍历 EPROCESS 链表，可能很慢
5. **内核模块**：需要手动指定基址和大小
6. **guest→root 转换测不到，必须靠补偿**：账本只能测「上一次 exit → 本次 entry」
   的 root 时间；从最后一条 guest 指令到硬件写 `exit_tsc` 之间的转换耗时在 root
   侧无法观测。目标进程量 `rdtsc; cpuid; rdtsc` 时，两次读之间夹着两次退出
   （`cpuid` 与第二次 `rdtsc`），handler 与 entry 窗口都已扣掉，剩下的残差就是
   **这两次转换之和**：

   ```text
   delta = (TSC_第二次读 − host_time) − (TSC_第一次读 − host_time)
         = 两次退出之间 guest 自己执行的时间 + Δ_cpuid + Δ_rdtsc
   ```

   所以补偿值是「实测残差 / 2」（hypercall `0x204`），且**按机器实测**：同一台
   机器上是否嵌套、CPU 代次都会改变 `Δ`，写死会偏。补偿按「命中退出」统一计，
   不区分退出类型，因此只是近似——它对 pafish 这类「两次指令模拟退出」的检测
   刚好对上，对夹着 EPT violation / 中断的窗口则会有偏差；
7. **隐藏进程的时钟会比墙钟慢**：它不再“看到”自己的 VM-exit 时间。这是本机制
   的目的，但也会让该进程内的超时/定时器整体偏慢。

## 11. 参考

- Windows EPROCESS 结构：`_EPROCESS` in WinDbg
- TSC offsetting 原理：Intel SDM Vol.3C, Chapter 24
- Hypervisor 架构：`AGENTS.md` §9.8

---

**文档版本**：1.1  
**最后更新**：2026-11-04  
**状态**：设计完成，实现进行中（真机验证 + 退出补偿标定）
