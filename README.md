# Noumen

[English](README_en.md)

noumen 是一个运行在 x86_64 上的轻量 Type-1 hypervisor，使用 Rust `no_std` 编写，同时支持 Intel VMX/EPT 和 AMD SVM/NPT。

当前仓库的加载器是 Windows 内核驱动：驱动负责分配 nonpaged 内存、构造 `KernelArgs` 并调用入口；hypervisor 核心只依赖 x86_64 虚拟化扩展，负责 CPU 虚拟化、EPT/NPT、hook、日志和内存原语。

新 agent 请先读 [AGENTS.md](AGENTS.md)。

[screenshot1](./imgs/s1.png) [screenshot2](./imgs/s2.png)

## 能力

- Intel VMX：VMCS、EPT、VPID、MSR bitmap、MTRR、EPT hook / EPT 原生断点 / HideSelf；
- AMD SVM：VMCB、NPT、MSRPM、NPT hook / HideSelf（NPT hook 当前为非完整实现，且没有 NPT 原生断点）；
- 统一 hypercall ABI，两个后端各自分发；
- `MEM_*` 批量读写/翻译/扫描/物理区间/统计；
- COM1 串口（显式开启）、日志环、panic 明细。

## Feature

| feature | 默认 | 作用 |
| --- | --- | --- |
| `serial-console` | 否 | COM1 串口输出；需显式开启 |
| `buffer-log` | 是 | 512 条日志环（约 76 KiB）；loader 提供，或 `log_base == 0` 时从页池分配 |
| `panic-detail` | 是 | panic 寄存器快照、KernelArgs、符号回溯 |
| `vmx-ept` | 是 | Intel 后端、identity EPT、EPT hook/mem |
| `svm-npt` | 是 | AMD 的 identity NPT、NPF、NPT hook/mem |
| `mem` | 是 | 通用 `MEM_*` 服务；后端各自实现 |

注意：

- `mem` 不再隐含 EPT。使用 `--no-default-features` 时，要显式加上 `vmx-ept` 或 `svm-npt`。
- `vmx-ept` 关闭时 VMX 仍可运行，但 EPT hook/mem 不编译，相关 hypercall 返回 `RAX = 3`。
- `svm-npt` 关闭时，SVM 核心仍编译，仅关闭 NPT/NPF/NPT hook/mem/HideSelf；guest 在宿主页表上运行，NPT 相关 hypercall 返回 `3`。
- 关闭 `serial-console` 和 `buffer-log` 后，`klog!` 仍保留调用点（避免 unused 警告），但不会输出。

### 关于Windows串口输出

Windows 启动后会占用串口，导致`serial-console`无效，删除`C:\WINDOWS\System32\drivers\serial.sys`。

## 构建

仓库布局（构建脚本按这个布局找东西）：

```text
build.py                 hypervisor + 两个 loader（或用 --flavor 选一个）
build_deviceio.py        hypervisor + kernel/deviceio（设备 IO 版）
build_kdmapper.py        hypervisor + kernel/kdmapper（手动映射版）
hypervisor/              Rust hypervisor crate（产物 `noumen` ELF）
loader/windows/kernel/   两个内核驱动：deviceio/、kdmapper/（镜像内嵌）
loader/windows/um/       两个前端：uapp/（deviceio 专用）、nsvr/（kdmapper 专用）
tools/rdtsc_probe/       RDTSC 时序探针
```

产物统一落到 `target/<flavor>/`：

```text
target/deviceio/   noumen, noumen_loader.sys, uapp.exe
target/kdmapper/   noumen, noumen_kdmapper.sys, nsvr.exe
```

```powershell
# 默认 Release，构建两版 loader
python build.py

# 只构建其中一版
python build_deviceio.py
python build_kdmapper.py

# Debug，跳过驱动
python build.py --debug --skip-driver

# 无串口
python build.py --no-serial

# 查看所有可用 feature
python build.py --list-features

# 显式给出全部默认 feature（加 serial-console 可同时开串口）
python build.py --features panic-detail,buffer-log,vmx-ept,svm-npt,mem

# 顺带把 RDTSC 时序探针（tools/rdtsc_probe/）一起拷进暂存目录
python build.py --with-probe

# 清理中间产物（cargo target + MSBuild build 目录）
python build.py --clean-only

# 指定 nsvr 的默认 hypercall key（缺省 0x10F2C）
python build.py --flavor kdmapper --hypercall-key 0x2b3d
```

`build_kdmapper.py` 会在调用 MSBuild 之前，用刚编译出来的 ELF 重新生成
`loader/windows/kernel/kdmapper/noumen_data.h`（镜像没变时不改写文件，避免
无谓重编驱动）。

只构建 hypervisor 时可以在 `hypervisor/` 下直接用 cargo：

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

产出：

```text
hypervisor\target\x86_64-unknown-none\<profile>\noumen
```

构建工具链要求：

- nightly Rust，带 `build-std`；
- Python 3.10+，仅使用标准库，用于 `build*.py` 和宿主测试脚本；
- LLVM `ld.lld.exe`，路径在 `.cargo/config.toml` 中，默认 `C:\Program Files\LLVM\bin\ld.lld.exe`；
- clang（`build.rs` 会把 `TMP/TEMP` 指到 `OUT_DIR`）；
- Visual Studio + WDK（MSBuild），用于两个 loader 驱动。

常见坑：

- 当前 nightly 不自带 `rust-lld`，不要改回 `rust-lld`；
- 不要使用 `.cargo-target`，所有产物只放仓库 `target/`；
- feature 组合要分别验证：默认、release、`vmx-ept` only、`svm-npt` only、无 feature。

## 测试

```powershell
# EPT hook 纯逻辑
python tests/test-ept-core.py

# SVM 纯逻辑
python tests/test-svm-core.py

# mem 纯逻辑
python tests/test-mem-core.py
```

测试脚本在 `tests/`，不会在裸机 target 上直接跑；`tests/svm-core-tests` 是独立的宿主 harness。

硬件路径（EPT violation、`#BP`、MTF、VMRUN、NPT TLB、NPF）必须上机验证，步骤见 [AGENTS.md](AGENTS.md) 第 20 节。

## Hypercall

寄存器约定：

```text
RAX = loader key
RCX = command
RDX/R8/R9 = 参数
RAX = 返回值
```

- key 错误：注入 `#UD`；
- 未知命令：VMX 注入 `#UD`，SVM 返回 `RAX = 3`；
- 后端不支持的命令：返回 `RAX = 3`；
- 命令编号是 ABI，只能追加，不能重排。

共享定义在 [`src/arch/x86_64/hypercall.rs`](src/arch/x86_64/hypercall.rs)；分发在：

- [`src/arch/x86_64/vmx/hypercall/`](src/arch/x86_64/vmx/hypercall/)；
- [`src/arch/x86_64/svm/hypercall/`](src/arch/x86_64/svm/hypercall/)。

命令族：

- `1..=4`：日志、ping、Stop；
- `5..=7`、`9..=11`、`13..=22`、`0x100`：hook、EPT 原生断点、自省；SVM 没有 NPT 原生断点路径；
- `0x20..=0x24`：`MEM_*`；
- `0x30..=0x3a`：SVM 专属。

## Hook 模型

### VMX/EPT

- guest 真实物理页不写入 `0xCC` 或 detour；
- fake 页 execute-only，数据读走真实页；
- 入口用 `mov r11, imm64; jmp r11`，避免读 execute-only 页；
- stolen 字节写入 origin/trampoline，末尾跳回 `target + stolen`；
- 所有核应用新 EPT 状态并 retire 后，才释放旧 origin；
- `STOP` 前恢复真实页。

详见 [AGENTS.md](AGENTS.md) 第 19 节。

### SVM/NPT

> **当前状态：非完整实现。**  
> NPT 没有 EPT 的 execute-only 语义，目前只能用 `OriginalExec` / `ShadowNx` / `ShadowExec` 三种 leaf 视图，在数据窗口和执行窗口之间按时间片切换；这不等价于 EPT 的完全分离，不能宣称“完全不可见”。
>
> SVM 没有实现 NPT 原生断点：`UserBreakpoint`（kind 4）返回 `Unsupported`，等待协议没有对应的断点安装路径。

当前实现：

- identity NPT 默认 2 MiB 大页，hook 页 split 到 4 KiB；
- 数据读和执行共用同一个 leaf，命中 hook 后先切数据视图，取指时再切执行视图；
- 每次 exit 恢复 `ShadowNx`，切换 leaf 后失效 NPT TLB；
- `STOP` 前恢复所有真实页，否则 `EFER.SVME=0` 后 guest 会 `0x3B`。

后续完整实现方向：

- 使用**双页表**（数据 NPT 与执行 NPT 两个根）分离数据视图和执行视图；
- 不再依赖同一 leaf 的时间片切换，目标是达到与 EPT 同级的隐蔽性；具体切换和 TLB 策略在实现时确定。

详见 [AGENTS.md](AGENTS.md) 第 20 节。

## 内存服务

`MEM_*` 提供：

- `MEM_QUERY`：ABI 版本、批量上限、直接映射大小；
- `MEM_BATCH`：guest 请求环批量读/写/翻译；
- `MEM_SCAN`：分段扫描；
- `MEM_MAP`：物理区间；
- `MEM_STATS`：每 CPU 统计。

VMX 使用 EPT 视图，SVM 使用 identity NPT + 直接映射。失败通过 `RAX = MemError` 返回，不注入异常。

详见 [AGENTS.md](AGENTS.md) 第 21 节。

## 日志

- 默认只有日志环；显式开启 `serial-console` 才有 COM1；
- 日志环 512 条，每条 152 字节；
- 环由 loader 分配并通过 `KernelArgs.log_base/log_size` 传入；`log_base == 0`（`log::init(0, _)`）时 hypervisor 从页池分配内部环，此时不要求 loader 提供日志区，guest 用 `FLUSH_LOGS`(1) 取走记录；
- 写入用 seqlock；读取方读 `sequence`、复制、再读，变化则重试；
- `id` 断号表示记录被覆盖；
- no-serial 镜像由 loader 周期线程落盘，崩溃转储搜索 `noumen.hv.log`。

## 文档

仓库保留以下长期文档：

- [README.md](README.md)：中文项目概览、feature、构建、日志；
- [README_en.md](README_en.md)：英文项目概览；
- [AGENTS.md](AGENTS.md)：架构、ABI、全部坑、EPT/SVM/mem 细节。

## 已知限制

- 宿主测试不覆盖硬件虚拟化路径；
- NPT 当前为非完整实现：没有 execute-only，NPT hook 依赖时间片视图切换，隐蔽性弱于 EPT；后续以双 NPT 页表作为完整实现；
- 单核模式下只有被虚拟化的 CPU 能执行 hypercall。

## 鸣谢

- [wbenny/hvpp](https://github.com/wbenny/hvpp)
- [jonomango/hv](https://github.com/jonomango/hv)
- [mikasaw/svmb](https://github.com/mikasaw/svmb)

QQ群: 1014181247
