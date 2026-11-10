# noumen loader

Windows 内核驱动。加载后只建立 `\\.\Noumen` 通信设备，不会自动启动 hypervisor。r3 调用 `StartHypervisor` 后，驱动才会读取镜像文件（默认 `C:\noumen`）并初始化各核心。

## 通信方式

- 设备：`\\.\Noumen`
- IOCTL：`NOUMEN_IOCTL_EXECUTE`，值 `0x00222000`
- 协议版本：`NOUMEN_PROTOCOL_VERSION`，当前为 3
- 输入：16 字节 `NOUMEN_REQUEST_HEADER` + 命令负载
- 输出：16 字节 `NOUMEN_RESPONSE_HEADER` + 命令返回数据

请求头：

```c
typedef struct _NOUMEN_REQUEST_HEADER
{
    ULONG ProtocolVersion;
    ULONG Command;
    ULONG InputLength;
    ULONG OutputLength;
} NOUMEN_REQUEST_HEADER;
```

响应头：

```c
typedef struct _NOUMEN_RESPONSE_HEADER
{
    ULONG ProtocolVersion;
    ULONG Command;
    NTSTATUS Status;
    ULONG OutputLength;
} NOUMEN_RESPONSE_HEADER;
```

`InputLength` 是负载字节数。`OutputLength` 是希望写入的负载字节数，填 0 表示使用输出缓冲区的剩余容量。驱动同时受实际缓冲区大小限制，最大输出负载为 1 MB。

`DeviceIoControl` 返回成功只说明请求头合法。命令执行结果要看 `ResponseHeader.Status`，命令失败时 IOCTL 仍可能返回成功。请求头不合法（版本不对、长度越界、输入或输出不足 16 字节）时，IOCTL 直接返回错误。

用户态包含 `command.h` 即可，`public.h` 已经由它包含。

最小调用示例：

```c
typedef struct
{
    NOUMEN_REQUEST_HEADER Header;
    NOUMEN_PING Ping;
} PING_REQUEST;

typedef struct
{
    NOUMEN_RESPONSE_HEADER Header;
    NOUMEN_PING_RESPONSE Ping;
} PING_RESPONSE;

PING_REQUEST req =
{
    {
        NOUMEN_PROTOCOL_VERSION,
        NoumenCommandPing,
        sizeof(NOUMEN_PING),
        sizeof(NOUMEN_PING_RESPONSE)
    },
    { 0x1234, 0 }
};

PING_RESPONSE rsp = { 0 };
DWORD bytes = 0;

DeviceIoControl(hDevice,
                NOUMEN_IOCTL_EXECUTE,
                &req,
                sizeof(req),
                &rsp,
                sizeof(rsp),
                &bytes,
                NULL);
```

命令值：

| 值 | 命令 | 输入 | 输出 |
|---|---|---|---|
| 0 | Ping | `NOUMEN_PING`，8 字节 | `NOUMEN_PING_RESPONSE`，8 字节 |
| 1 | GetInfo | 无 | `NOUMEN_INFO`，72 字节 |
| 2 | GetKernelArgs | 无 | `NOUMEN_KERNEL_ARGS`，80 字节 |
| 3 | SetKernelArgs | `NOUMEN_KERNEL_ARGS`，80 字节 | 无 |
| 4 | SetImagePath | 8 字节头 + UTF-16 路径 | 无 |
| 5 | SetLogPath | 8 字节头 + UTF-16 路径 | 无 |
| 6 | StartHypervisor | `NOUMEN_START_HYPERVISOR`，8 字节 | 无 |
| 7 | StopHypervisor | 无 | 无 |
| 8 | HypercallPing | `NOUMEN_HYPERCALL_PING_REQUEST`，16 字节 | `NOUMEN_HYPERCALL_PING_RESPONSE`，16 字节 |

## Ping (0)

```c
typedef struct
{
    ULONG Value;
    ULONG Reserved;
} NOUMEN_PING;

typedef struct
{
    ULONG Value;
    ULONG Magic;
} NOUMEN_PING_RESPONSE;
```

用于确认驱动还在响应。`Value` 原样带回，`Magic` 固定为 `NOUMEN_PING_MAGIC`，值 `0x504F4E47`。

返回：
- `STATUS_SUCCESS`：正常。
- `STATUS_INVALID_PARAMETER`：输入不足 8 字节。
- `STATUS_BUFFER_TOO_SMALL`：输出不足 8 字节。

Ping 不依赖 hypervisor，启动前也能用。

## GetInfo (1)

无输入，输出 `NOUMEN_INFO`，72 字节。

| 字段 | 含义 |
|---|---|
| `ProtocolVersion` | 协议版本 |
| `State` | 0 Unloaded，1 Loading，2 Running，3 Failed |
| `LastStatus` | 最近一次启动的结果 |
| `CpuCount` | 生效的核心数 |
| `StartProcessor` | 实际运行 `_start` 的核心号，启动前为 0 |
| `SingleCore` | 1 = 单核模式（只虚拟化 `StartProcessor`），0 = 全核模式 |
| `KernelBase` / `KernelSize` | ELF 映像的基址和大小 |
| `EntryPoint` | ELF 入口地址 |
| `SymbolBase` / `SymbolCount` | 符号表地址和条目数 |
| `LogSize` | 共享日志环大小，固定 77872 |
| `HypercallKey` | 传给 hypervisor 的 key，默认 `0x10F2C` |

启动前大部分地址字段为 0。核心号从 `StartProcessor` 读取，单独的 GetCoreId 命令已经去掉。

返回：
- `STATUS_SUCCESS`：正常。
- `STATUS_BUFFER_TOO_SMALL`：输出不足 72 字节。

## GetKernelArgs (2)

无输入，输出 `NOUMEN_KERNEL_ARGS`，80 字节。

- 没 Set 过也没启动过：全 0。
- Set 过但没启动：返回 Set 的值。
- 启动后：返回实际传给 `_start` 的那一份，包含驱动在启动时补上的字段。

返回：
- `STATUS_SUCCESS`：正常。
- `STATUS_BUFFER_TOO_SMALL`：输出不足 80 字节。

## SetKernelArgs (3)

输入 `NOUMEN_KERNEL_ARGS`，80 字节。无输出。

调用成功后，驱动把这份结构视为已初始化，下一次 Start 不再自动填任何字段。结构包括 `SystemCr3`、`SystemCr3Virt`、`KernelBase/Size`、符号表、字符串表、`LogBase/Size`、`CpuCount` 和物理区间（`PhysMapBase/Count`，可为 0）。

这里没有“只改一个字段”的接口。调用 Set 之后要么给完整结构，要么就不要调用。只填一部分会让其余字段保持 0，启动时很可能直接蓝屏。

`CpuCount` 仍会被限制在 1 到活动核心数之间，启动时驱动会把它改成最终生效值。

返回：
- `STATUS_SUCCESS`：已保存。
- `STATUS_UNSUCCESSFUL`：已经调用过 `_start`，不能再改。需要卸载驱动重新加载。
- `STATUS_INVALID_PARAMETER`：输入不足 80 字节。

只有 `_start` 还没执行时可以改。启动失败但失败发生在 `_start` 之前，比如镜像文件打不开，可以改完再试。

## SetImagePath (4)

输入是 8 字节头加 UTF-16 路径，结构为 `NOUMEN_SET_IMAGE_PATH`，布局和 `NOUMEN_SET_LOG_PATH` 一样。

`Length` 是路径字节数，不含结尾 NUL，必须是 2 到 520 之间的偶数。实际输入可以只发 `8 + Length` 字节。

路径格式与 SetLogPath 相同：

- `C:\noumen` 或 `C:/noumen`：转成 `\??\C:\noumen`
- `\??\...`、`\Device\...`：原样使用
- 相对路径、普通 UNC、内嵌 NUL：拒绝

默认 `\??\C:\noumen`。只能在 `_start` 执行前设置。设置后会丢弃之前通过 `NoumenHypervisorLoadImageFromMemory` 预加载的映像，Start 改回从文件加载。换镜像也会清掉 KernelArgs 的初始化标志，Start 会按新映像重新填充参数；需要自定义参数时，在 SetImagePath 之后再调用 SetKernelArgs。

返回：
- `STATUS_SUCCESS`：路径已记录。
- `STATUS_INVALID_PARAMETER`：长度、编码或路径格式不对。
- `STATUS_UNSUCCESSFUL`：已经调用过 `_start`，不能再改。

## SetLogPath (5)

输入是 8 字节头加 UTF-16 路径：

```c
typedef struct _NOUMEN_SET_LOG_PATH
{
    ULONG Length;
    ULONG Reserved;
    WCHAR Path[260];
} NOUMEN_SET_LOG_PATH;
```

`Length` 是路径字节数，不含结尾 NUL，必须是 2 到 520 之间的偶数。实际输入可以只发 `8 + Length` 字节，不必把 260 个 WCHAR 全部发完。

路径格式：

- `C:\logs\a.log` 或 `C:/logs/a.log`：驱动转成 `\??\C:\logs\a.log`
- `\??\...`、`\Device\...`：原样使用
- 相对路径、普通 UNC、带内嵌 NUL 的路径：拒绝

默认路径是 `\??\C:\noumen.log`。命令只检查格式并记录，不检查目录是否存在。目录不存在时，下一次写文件会失败。

可以在任何时候调用，包括启动前和运行中。运行中改路径，下一次周期 dump 生效。文件采用覆盖写入，每次 dump 重写整个文件。

返回：
- `STATUS_SUCCESS`：路径已记录。
- `STATUS_INVALID_PARAMETER`：长度、编码或路径格式不对。
- `STATUS_DEVICE_NOT_READY`：日志模块未初始化。正常加载不会出现。

## StartHypervisor (6)

输入：

```c
typedef struct _NOUMEN_START_HYPERVISOR
{
    ULONG SingleCore;
    ULONG RandomizeCore;
} NOUMEN_START_HYPERVISOR;
```

两个字段都按非 0 为真处理。

- `SingleCore = 1`：只对选中的核心执行 `_init`。
- `SingleCore = 0`：对所有生效核心执行 `_init`。
- `RandomizeCore = 0`：选 core 0。
- `RandomizeCore = 1`：在 `0 .. CpuCount-1` 中随机选一个核心。

生效核心数默认是活动核心数。如果 SetKernelArgs 设置过 `CpuCount` 且小于活动核心数，则使用该值。

启动线程会先绑定到选中的核心，把实际核心号写入 `StartProcessor`，然后执行 `_start` 和逐核 `_init`。启动在系统线程里完成，`SystemCr3` 取的是 System 进程的 CR3。

返回：
- `STATUS_SUCCESS`：已经运行，或这次启动成功。Running 状态下重复调用直接返回成功。
- `STATUS_UNSUCCESSFUL`：之前已经调用过 `_start` 且失败，不能再次启动。
- 其他失败码：镜像文件打不开、内存不足、`_start` 或 `_init` 返回非 0 等。

启动调用会阻塞到所有核心初始化完成，期间不要重复发送 Start。启动前确认镜像路径存在，默认 `C:\noumen`。

失败发生在 `_start` 之前时，可以修文件或改 KernelArgs 后重试。失败发生在 `_start` 之后时，驱动不再允许重试，建议重启系统。

成功且 `NOUMEN_ENABLE_LOGGER = 1` 时，会立刻 dump 一次日志并启动周期线程。为 0 时跳过这一步，但启动失败的诊断文件仍会写。

## StopHypervisor (7)

无输入输出。成功返回 `STATUS_SUCCESS`；hypervisor 不在运行状态时返回 `STATUS_INVALID_DEVICE_STATE`。

它逐个把已经虚拟化的 CPU 绑核、发 `VMCALL(STOP)`。hypervisor 在那个核上：

1. 把 guest 的 RIP/RSP/RFLAGS/CR3 从 VMCS 取出来（`VMXOFF` 之后就再也读不到了）；
2. 把 root 里换掉的 PAT 写回硬件；
3. `VMXOFF`，切回 guest 的 CR3，恢复 GPR/DR0–3/CR2/CR8，直接在 root 里跳回 VMCALL 的下一条指令。

所有核停完之后状态回到 `Unloaded`，镜像留在内存里等驱动卸载时释放。**再次 Start 需要重新加载驱动**：每核的初始化状态（`GlobalVcpu.initialized`）不可重用，`_init` 会直接拒绝。

停之前驱动会先关闭日志线程的 ping —— 它每秒对“当前 CPU”发一次 VMCALL，而核退出 VMX 之后 VMCALL 会变成 `#UD`；停止循环结束后再停线程。

停止过程不做 `VMCLEAR`：`VMXOFF` 本身就离开 VMX operation，VMCS/VMXON 区域随镜像一起释放。

仍然不要用“直接卸载驱动”代替 Stop，理由同上：卸载会把 hypervisor 的代码和 host 状态一起拆掉，下一次 VM exit 就会出问题。

## HypercallPing (8)

> 协议版本 2 起，请求是 16 字节（`Nonce` + `Processor` + `Reserved`）。旧版驱动/ uapp 混用时 IOCTL 会在请求头检查处直接返回 `STATUS_REVISION_MISMATCH`，不会再进入下面的逻辑。

输入：

```c
typedef struct _NOUMEN_HYPERCALL_PING_REQUEST
{
    UINT64 Nonce;
    ULONG Processor;
    ULONG Reserved;
} NOUMEN_HYPERCALL_PING_REQUEST;
```

输出：

```c
typedef struct _NOUMEN_HYPERCALL_PING_RESPONSE
{
    UINT64 Reply;
    ULONG Processor;
    ULONG Reserved;
} NOUMEN_HYPERCALL_PING_RESPONSE;
```

`Processor` 是目标 CPU，或者 `NOUMEN_HYPERCALL_PROCESSOR_ANY`（`0xFFFFFFFF`）：

- `NOUMEN_HYPERCALL_PROCESSOR_ANY`：由驱动选核。单核模式用 `StartProcessor`（只有它执行过 `VMXON`）；多核模式优先用处理本次 IOCTL 的当前核，当前核不在 `0 .. CpuCount-1` 时回退到 `StartProcessor`。
- 具体 CPU 号：单核模式必须等于 `StartProcessor`，多核模式必须小于 `CpuCount`，否则返回 `STATUS_INVALID_PARAMETER`。

驱动在 `g_HypervisorLock` 内确认状态为 Running，选核后把当前线程绑到该核，执行一次 `VMCALL(PING, Nonce)`，恢复亲和性，再把原始应答和实际使用的核心号返回。`Reply` 正常应为 `0x504F4E47 ^ Nonce`；不是这个值说明 `VMCALL` 没有真正产生 VM exit（例如核心已经不在 VMX non-root），这属于诊断信息，驱动不会把它改写成命令错误。

为什么需要这个命令：`VMCALL` 只有在 VMX non-root operation 下才是 VM exit。单核模式只对 `StartProcessor` 执行 `_init`/`VMXON`；在其它任何核上执行 `VMCALL` 会 `#UD`，而 r3 的 `#UD` 会终止进程。这正是“单核启动后 Hypercall Ping 概率闪退”的原因。把选核和 `VMCALL` 放进驱动，单核/多核都只会在真正跑 guest 的核上执行，也不会和 Stop 的逐核 `VMXOFF` 交错。

返回：
- `STATUS_SUCCESS`：已经执行 `VMCALL` 并拿到应答（不保证应答匹配，调用方自己比较）。
- `STATUS_INVALID_DEVICE_STATE`：hypervisor 不在 Running 状态。
- `STATUS_INVALID_PARAMETER`：输入不足 16 字节，或指定的 `Processor` 不是当前模式下已虚拟化的核。
- `STATUS_UNSUCCESSFUL`：线程在重试窗口内没有迁移到选中的核，驱动拒绝执行 `VMCALL`（不会再返回 `STATUS_DEVICE_NOT_READY`）。
- `STATUS_BUFFER_TOO_SMALL`：输出不足 16 字节。

## NoumenHypervisorLoadImageFromMemory

驱动内部接口，目前没有对应命令，留给后续从 r3 传 ELF 使用。

```c
NTSTATUS
NoumenHypervisorLoadImageFromMemory(
    const VOID *Data,
    ULONG Size
    );
```

参数：
- `Data`：完整的 ELF 文件内容。
- `Size`：字节数，不能为 0。

返回：
- `STATUS_SUCCESS`：映像已加载，Start 会直接使用它，不再读取文件。
- `STATUS_INVALID_PARAMETER`：`Data` 为空或 `Size` 为 0。
- `STATUS_UNSUCCESSFUL`：已经调用过 `_start`，不能再换映像。
- `STATUS_INVALID_IMAGE_FORMAT` / `STATUS_INSUFFICIENT_RESOURCES`：ELF 解析失败或内存不足。

说明：
- 只允许在 `_start` 之前调用。
- 实际映射由 ELF 模块的 `NoumenElfLoadFromMemory` 完成；这个接口负责把映像存进 hypervisor 状态并让 Start 使用。
- ELF 会被复制并映射到内核池，函数返回后 `Data` 可以释放。
- 加载成功会清掉 `KernelArgsInitialized` 并清零 `KernelArgs`，Start 按新映像重新填充；需要自定义参数时，在加载后再调用 `SetKernelArgs`。
- 调用 `SetImagePath` 会释放这份内存映像并切回文件模式。
- Start 会检查 `ImageBase` 和 `EntryPoint`，无效时返回 `STATUS_INVALID_IMAGE_FORMAT`。

## 其他

- 设备默认只允许管理员打开。
- `NOUMEN_ENABLE_SERIAL`：默认 0，改成 1 编译 COM1 串口代码。
- `NOUMEN_ENABLE_LOGGER`：默认 1，改成 0 不启动成功后的日志 dump 和周期线程。
- 构建 Debug 或 Release x64，产物在 `build\x64\<Configuration>\noumen_loader.sys`。
