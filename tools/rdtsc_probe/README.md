# rdtsc_probe

测量 `rdtsc; cpuid; rdtsc` 的时序，用于观察 CPUID 路径在 guest 侧的延迟。输出每轮的
min/avg/max 和 pafish 风格 verdict；**它不检测虚拟化签名**，只测时序。

## 用法

```powershell
cd tools/rdtsc_probe
cargo build --release
.\target\x86_64-pc-windows-msvc\release\rdtsc_probe.exe
```

每轮采样 10 次，每次间隔 500 ms；每轮结束后等回车再开始下一轮，便于对比不同配置。
`Ctrl+C` 随时退出；stdin 关闭时跑完当前轮就结束。

判定阈值与 pafish 一致：

```text
avg - 1 > 998  ->  TRACED
否则           ->  OK
```

`TRACED` 表示这轮采样里 CPUID 开销明显偏高；`OK` 只表示低于阈值，不代表没有虚拟化，
也不代表某个隐藏功能已启用。

## 一起构建

```powershell
python build.py --with-probe
```

会把 `rdtsc_probe.exe` 一起拷到 `target/deviceio/` 和 `target/kdmapper/`。

## 注意

- 输出故意用英文：中文 Windows 控制台默认 cp936，中文可能乱码；
- 探针只测 CPUID/RDTSC 时序，不读取 hypervisor 内部状态；
- 宿主机负载、CPU 频率变化和调度都会影响结果，建议多轮取中位数观察。
