//! RDTSC 时序探针：打印自身 PID，**每轮结束都停下来等回车**，再跑下一轮。
//!
//! 采样/判定逻辑照着 pafish 的时序检测写：每轮 10 次采样，每次 `rdtsc; cpuid; rdtsc`
//! 取差值，间隔 500 ms；每轮 `avg = sum / 10`，`avg - 1 > 998` 判为 `TRACED`。
//!
//! 用途：先把本进程 PID 配到 hypervisor 的 TSC 面板，跑一轮看基线；然后在面板里
//! 设/清目标 PID，再按回车跑下一轮，对比 verdict 变化。Ctrl+C 随时退出。
//!
//! 输出一律英文：探针跑在中文 Windows 的 cp936 控制台上，中文会乱码。

use std::io::{self, Write};
use std::time::Duration;

use core::arch::x86_64::{__cpuid, _rdtsc};

#[link(name = "kernel32")]
unsafe extern "system" {
    /// 拿本进程 PID；只调这一个 API，不引第三方依赖。
    fn GetCurrentProcessId() -> u32;
}

/// 每轮的采样次数（与 pafish 一致）。
const SAMPLES: usize = 10;
/// 每次采样之间的间隔。
const SLEEP_MS: u64 = 500;
/// 判定阈值：`avg - 1` 超过它就认为被测到虚拟化。`0x3E6 == 998`。
const TRACED_THRESHOLD: u64 = 0x3E6;

/// 跨轮累计，方便看隐藏开关切换后 verdict 是否翻转。
struct Totals {
    ok: u64,
    traced: u64,
    best_avg: u64,
    worst_avg: u64,
}

impl Totals {
    const fn new() -> Self {
        Self {
            ok: 0,
            traced: 0,
            best_avg: 0,
            worst_avg: 0,
        }
    }
}

fn main() {
    let pid = unsafe { GetCurrentProcessId() };
    let exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| String::from("<unknown>"));

    println!("rdtsc_probe");
    println!("pid  = {pid}");
    println!("exe  = {exe}");
    println!("tsc  = {:#x} (now)", unsafe { _rdtsc() });
    println!();
    println!("1) In the hypervisor TSC tab set the target PID to {pid}");
    println!("2) Press Enter to run one round ({SAMPLES} samples x {SLEEP_MS} ms)");
    println!("   after each round it waits for Enter again; Ctrl+C to quit");

    if !wait_for_enter("start round 1") {
        return;
    }

    let mut totals = Totals::new();
    let mut round: u64 = 1;

    loop {
        sample_round(round, &mut totals);
        round = round.wrapping_add(1);

        // 每轮结束后停下来，让人有机会在 hypervisor 面板里改配置（设/清目标 PID）
        // 再手动跑下一轮。
        if !wait_for_enter(&format!(
            "press Enter for round {round} (or Ctrl+C to quit)"
        )) {
            println!("stdin closed, stopping after {} round(s).", round - 1);
            return;
        }
    }
}

/// 打印提示并等待一行输入。返回 `false` = stdin 已关闭/读失败（该退出）。
fn wait_for_enter(prompt: &str) -> bool {
    print!("{prompt} ... ");
    io::stdout().flush().ok();

    let mut line = String::new();

    match io::stdin().read_line(&mut line) {
        // 0 = EOF：没有交互终端（重定向/服务），不能死等，直接收尾。
        Ok(0) => {
            println!("<eof>");
            false
        }
        Ok(_) => true,
        Err(_) => false,
    }
}

/// 跑一轮采样并打印结论。读 TSC 不会失败，所以这里不返回错误。
fn sample_round(round: u64, totals: &mut Totals) {
    let mut sum: u64 = 0;
    let mut min: u64 = u64::MAX;
    let mut max: u64 = 0;

    println!();
    println!("--- round {round} ---");

    for index in 0..SAMPLES {
        let start = unsafe { _rdtsc() };

        // CPUID 是序列化指令：它会把流水线清空，这里刚好用来放大 VM-exit 的影响。
        __cpuid(0);

        let end = unsafe { _rdtsc() };
        let delta = end.wrapping_sub(start);

        println!("sample[{index}] = {delta} ticks");

        sum = sum.wrapping_add(delta);
        min = min.min(delta);
        max = max.max(delta);

        std::thread::sleep(Duration::from_millis(SLEEP_MS));
    }

    let avg = sum / SAMPLES as u64;

    println!("avg = {avg}, min = {min}, max = {max}");

    if round == 1 {
        totals.best_avg = avg;
        totals.worst_avg = avg;
    } else {
        totals.best_avg = totals.best_avg.min(avg);
        totals.worst_avg = totals.worst_avg.max(avg);
    }

    let traced = avg.saturating_sub(1) > TRACED_THRESHOLD;
    let verdict = if traced { "TRACED" } else { "OK" };

    if traced {
        totals.traced += 1;
    } else {
        totals.ok += 1;
    }

    println!("pafish verdict: {verdict}  (threshold: avg - 1 > {TRACED_THRESHOLD})");
    println!(
        "rounds so far: ok={} traced={}  avg range: {}..{}",
        totals.ok, totals.traced, totals.best_avg, totals.worst_avg
    );

    if traced {
        println!("hint: this PID is not hidden, or hiding is not enabled");
        println!("      (check for `TSC: rdtsc_exiting on` in the hypervisor log)");
    }

    io::stdout().flush().ok();
}
