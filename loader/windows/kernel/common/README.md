# kernel/common

`deviceio` 和 `kdmapper` 共享的内核静态库（`noumen_common_<flavor>.lib`）。

两个驱动只保留自己的入口和装载方式：

- `deviceio/`：`Driver.c`、`comm.c`、`command.c`、`serial.c`；
- `kdmapper/`：`main.c`、`noumen_data.h`。

其余代码在 common 中维护：

```text
common.h / Public.h       公共类型、池标签和 flavor 开关
command.h                 IOCTL/驱动命令 ABI
elf.h / elfloader.c/h     ELF 镜像装载
hypercall.c/h             hypercall 封装
hypervisor.c/h            镜像、KernelArgs、hook、HideSelf、日志
noumenlog.c/h             日志环和落盘
path.c/h                  NT 路径转换
elf_stub.asm              ELF 跳板
hypercall_stub.asm        VMCALL/VMMCALL 跳板
```

## flavor 差异

`Public.h` 根据 `NOUMEN_FLAVOR_KDMAPPER` 选择：

| 项目 | deviceio | kdmapper |
|---|---|---|
| 池标签 | `Noum`/专用 tag | `NDIS` |
| `NOUMEN_ENABLE_LOGGER` | 1 | 0 |
| 进程退出通知 | 注册 | 不注册 |
| `KernelArgs.LogBase` | 日志环 | 0 |

common 工程本身不定义驱动入口；`ConfigurationType` 为 `StaticLibrary`。
`common_deviceio.vcxproj` 和 `common_kdmapper.vcxproj` 编译同一份源码，
分别固定对应的 flavor；两个驱动的 `driver.vcxproj` 各自引用匹配的工程。

单独构建某个 flavor：

```powershell
msbuild common_deviceio.vcxproj /p:Configuration=Release /p:Platform=x64
msbuild common_kdmapper.vcxproj /p:Configuration=Release /p:Platform=x64
```

正常构建使用根目录的 `build.py` / `build_deviceio.py` / `build_kdmapper.py`，
不需要手动调用 MSBuild。
