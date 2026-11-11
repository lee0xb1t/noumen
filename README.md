# Noumen

[English](README_en.md)

noumen 是一个 x86_64 上的轻量 Type-1 风格 hypervisor，使用 Rust `no_std` 编写，同时支持 Intel VMX/EPT 和 AMD SVM/NPT。

仓库中的 loader 是 Windows 内核驱动：驱动负责分配 nonpaged 内存、构造 `KernelArgs` 并调用入口；hypervisor 核心只依赖 x86_64 虚拟化扩展，负责 CPU 虚拟化、EPT/NPT、hook、日志和内存原语。

[screenshot1](./imgs/s1.png) [screenshot2](./imgs/s2.png)

## 使用前注意

noumen 需要独占 VT-x/AMD-V。使用前关闭 VBS、内存完整性（HVCI）、Hyper-V、WSL2、
Windows Sandbox 等虚拟化功能；虚拟机内运行还需开启嵌套虚拟化。在“Windows 安全中心
→ 设备安全性 → 内核隔离”关闭内存完整性，并关闭上述虚拟化功能后重启；重启后用
`msinfo32` 确认 VBS 未启用。

## 能力

- Intel VMX：VMCS、EPT、VPID、MSR bitmap、MTRR、EPT hook、EPT 原生断点、HideSelf；
- AMD SVM：VMCB、NPT、MSRPM、NPT hook、HideSelf；NPT hook 为不完整实现，且没有 NPT 原生断点；
- VMX CPUID 全量原生透传；`hypervisor-msr` 单独控制外层 hypervisor MSR；
- 统一 hypercall ABI，两个后端分别分发；
- `MEM_*` 批量读写、翻译、扫描、物理区间和统计；
- 日志环、panic 明细，可选 COM1 串口。

## Feature

| feature | 默认 | 作用 |
| --- | --- | --- |
| `serial-console` | 否 | COM1 串口输出，需显式开启 |
| `buffer-log` | 是 | 512 条日志环；loader 提供，或从页池分配内部环 |
| `panic-detail` | 是 | panic 寄存器快照、KernelArgs、符号回溯 |
| `vmx-ept` | 是 | Intel 后端、identity EPT、EPT hook/mem |
| `native-bp` | 否 | VMX 原生断点、#BP/#CP/NMI 拦截；需要时显式开启，依赖 `vmx-ept` |
| `hypervisor-msr` | 否 | 透传外层 `0x40000000..=0x400000FF` MSR；默认注入 `#GP`，仅 VMware/嵌套环境开启 |
| `svm-npt` | 是 | AMD identity NPT、NPF、NPT hook/mem |
| `mem` | 是 | 通用 `MEM_*` 服务，后端各自实现 |

注意：

- `mem` 不隐含 `vmx-ept`/`svm-npt`；使用 `--no-default-features` 时要显式加后端 feature。
- 关闭 `vmx-ept`/`svm-npt` 时核心仍可编译，相关 hook/mem hypercall 返回 `RAX = 3`；VMX 的 EPT hook/HideSelf 不编译，`enable_vpid`/`unrestricted_guest` 也关闭。
- `native-bp` 默认关闭；需要 `UserBreakpoint`/`#BP`/`#CP`/NMI 时显式开启（依赖 `vmx-ept`）。只使用 `MEM_*`/HideSelf 时保持关闭，避免这些退出带来的 TSC 膨胀。
- `hypervisor-msr` 默认关闭；VMX CPUID 始终裸透传，这个 feature 只控制 `0x40000000..=0x400000FF` 的外层 MSR。默认严格注入 `#GP`，VMware/嵌套环境才开启透传。
- 同时关闭 `serial-console` 和 `buffer-log` 时，`klog!` 仍参与类型检查，但不输出。

使用 `serial-console` 时，Windows 会占用 COM1，解决办法是删除 `C:\WINDOWS\System32\drivers\serial.sys`。

## 构建

依赖：nightly Rust + `build-std`、Python 3.10+、LLVM `ld.lld.exe`、clang、Visual Studio + WDK。

```powershell
# 构建两版 loader（Release）
python build.py

# 只构建其中一版
python build_deviceio.py
python build_kdmapper.py
python build_kdmapper.py --hypervisor-msr   # VMware/嵌套环境

# kdmapper 构建会临时改写 noumen_data.h，编译结束自动恢复
# Debug，跳过驱动
python build.py --debug --skip-driver

# 清理中间产物
python build.py --clean-only
```

产物：

```text
target/deviceio/   noumen, noumen_loader.sys, uapp.exe
target/kdmapper/   noumen, noumen_kdmapper.sys, nsvr.exe, mcp-server.exe, test_mcp_server.exe
```

只构建 hypervisor：

```powershell
cd hypervisor
cargo build
cargo build --release
cargo build --no-default-features --features vmx-ept
cargo build --no-default-features --features svm-npt
cargo build --release --no-default-features
```

只使用 `MEM_*`/HideSelf、不需要原生断点时：

```powershell
cargo build --release --no-default-features --features buffer-log,vmx-ept,mem
```

产物在 `hypervisor/target/x86_64-unknown-none/<profile>/noumen`。工具链已由 `rust-toolchain.toml` 固定；`.cargo/config.toml` 中的 `ld.lld.exe` 路径需按机器修改。

## MCP hypercall 服务

kdmapper 构建会同时生成只调用 hypercall 的 MCP/REST 服务：

```powershell
.\target\kdmapper\mcp-server.exe
```

构建、REST 端点、MCP 工具、参数和安全边界统一见 [MCP.md](MCP.md)。

## 测试

```powershell
cd hypervisor
python tests/test-ept-core.py
python tests/test-svm-core.py
python tests/test-mem-core.py
```

宿主机测试不覆盖真实硬件路径；EPT violation、`#BP`、MTF、VMRUN、NPT TLB、NPF 需要上机验证。

## Hypercall

```text
RAX = loader key
RCX = command
RDX/R8/R9 = 参数
RAX = 返回值
```

- key 错误：注入 `#UD`；
- 未知/不支持的命令：返回 `RAX = 3`；
- 命令编号是 ABI，只能追加，不能重排。

命令族：

- `1..=4`：日志、ping、Stop；
- `5..=7`、`9..=11`、`13..=22`、`0x100`：hook、EPT 原生断点、自省；
- `0x20..=0x24`：`MEM_*`；
- `0x30..=0x3a`：SVM 专属；
- `0x200..=0x206`：保留编号（TSC 虚拟化已移除），返回 `3`。

## 日志

- 默认只有日志环，显式开启 `serial-console` 才有 COM1；
- 环为 512 条、每条 152 字节；`log_base == 0` 时由 hypervisor 从页池分配内部环，guest 用 `FLUSH_LOGS`(1) 取走记录；
- 读取方用 seqlock；`id` 断号表示记录被覆盖；
- no-serial 镜像由 loader 周期线程落盘，崩溃转储搜索 `noumen.hv.log`。

## 文档

- [DOCS.md](DOCS.md)：架构、ABI、全部实现细节和坑；
- [loader/windows/kernel/deviceio/README.md](loader/windows/kernel/deviceio/README.md)：deviceio IOCTL/命令 ABI；
- [MCP.md](MCP.md)：MCP/REST hypercall 服务的完整说明；
- [README_en.md](README_en.md)：英文概览；

## 已知限制

- 宿主测试不覆盖硬件虚拟化路径；
- VMX CPUID 全量裸透传会暴露宿主 VMX/hypervisor 位；noumen 仍拒绝 `CR4.VMXE`/`VMXON`；
- NPT 没有 execute-only，NPT hook 依赖时间片视图切换，隐蔽性弱于 EPT；
- 单核模式下只有被虚拟化的 CPU 能执行 hypercall。

## 鸣谢

- [wbenny/hvpp](https://github.com/wbenny/hvpp)
- [jonomango/hv](https://github.com/jonomango/hv)
- [mikasaw/svmb](https://github.com/mikasaw/svmb)

---

QQ群: 1014181247
