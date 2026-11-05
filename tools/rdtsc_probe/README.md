# rdtsc_probe

RDTSC 时序探针，用来验证 noumen 的 **TSC 选择性隐藏**（`AGENTS.md` §9.8）有没有真的
把 VM-exit 的 host 时间从 guest 视角抹掉。

采样逻辑照 pafish 的时序检测写，**每轮跑完停下来等回车**再开始下一轮：

```text
每一轮:
  for i in 0..10:
      t0 = rdtsc
      cpuid(0)        # 序列化，放大 VM-exit 影响
      t1 = rdtsc
      delta[i] = t1 - t0
      sleep 500ms

  avg = sum(delta) / 10
  verdict = (avg - 1 > 998) ? "TRACED" : "OK"
  打印本轮结论 + 累计 ok/traced 与 avg 区间
  等回车 → 下一轮
```

阈值 `0x3E6 == 998` 与 pafish 一致。Ctrl+C 随时退出；stdin 关闭（重定向/无终端）时
跑完当前轮就正常结束，不会死等。

## 用法

```powershell
# 构建
cd rdtsc_probe
cargo build --release

# 运行
.\target\x86_64-pc-windows-msvc\release\rdtsc_probe.exe
```

流程：

1. 探针打印 `pid = N`，停在 `start round 1 ...`；
2. 在 uapp 的 **TSC** 页把目标 PID 设成 `N`（下发 hypercall `0x200`）；
3. 按回车跑一轮，看 `pafish verdict`；
4. 停在 `press Enter for round N ...`，这时可以回面板里设/清目标 PID 改配置，
   再按回车跑下一轮对比。

## 输出示例

没有被隐藏时（本机未加载 hypervisor，仅作基线）：

```text
rdtsc_probe
pid  = 27912
exe  = ...\rdtsc_probe.exe
tsc  = 0x4e2704bf5ce8 (now)

1) In the hypervisor TSC tab set the target PID to 27912
2) Press Enter to run one round (10 samples x 500 ms)
   after each round it waits for Enter again; Ctrl+C to quit
start round 1 ... 
--- round 1 ---
sample[0] = 630 ticks
...
avg = 758, min = 446, max = 1148
pafish verdict: OK  (threshold: avg - 1 > 998)
rounds so far: ok=1 traced=0  avg range: 758..758
press Enter for round 2 (or Ctrl+C to quit) ... 
```

`TRACED` 说明这次 `cpuid` 的 VM-exit 时间落到了 guest 看到的 TSC 里；`OK` 说明要么
没被隐藏但开销本身很小，要么隐藏生效了。判断隐藏是否真的启用，以 hypervisor 日志里的
`TSC: rdtsc_exiting on` 为准。

## 注意

- 探针本身**不**检测虚拟化签名，只测时序；CPUID 相关检测不在这里。
- 隐藏生效时全系统 `RDTSC` 都会 VM-exit，忙等型程序会明显变慢；测完记得在 TSC 页
  点“清除”，`rdtsc_exiting` 会自动关掉。
- 输出故意用英文：中文 Windows 控制台默认 cp936，中文会乱码。

## 一起构建

`python build.py --with-probe` 会把 `rdtsc_probe.exe` 一起拷进
`target/deviceio/` 和 `target/kdmapper/`，方便连 loader 一起丢到测试机上。
