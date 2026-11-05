//! Guest 可见的 `IA32_APERF`/`IA32_MPERF`。
//!
//! 两者是 per logical processor 的自由计数器，VMX 不随 entry/exit 换入换出，
//! 直接放行会把 root 执行时间也暴露给 guest。
//!
//! **按需同步**：不在每次退出时累加，只在 guest 真读这两个 MSR 时把「上次基准
//! 以来的硬件增量」攒进 guest 视图。增量不是常量，就是那一刻两个计数器各自
//! 相对上次基准的差值；不读它们时零开销。

use crate::arch::x86_64::safe_routine::{self, HostExceptionInfo};

#[derive(Clone, Copy)]
pub struct VirtualPerfCounters {
    pub aperf: u64,
    pub mperf: u64,
    base_aperf: u64,
    base_mperf: u64,
    /// `CPUID.06H:ECX[0]`：硬件支持 APERF/MPERF。
    pub supported: bool,
}

impl VirtualPerfCounters {
    pub const fn new() -> Self {
        Self {
            aperf: 0,
            mperf: 0,
            base_aperf: 0,
            base_mperf: 0,
            supported: false,
        }
    }

    /// setup：探测支持并记下首个基准。
    pub fn init(&mut self) {
        self.supported = x86::cpuid::cpuid!(6, 0).ecx & 1 != 0;

        if self.supported {
            let (aperf, mperf) = read_hw();

            self.base_aperf = aperf;
            self.base_mperf = mperf;
        }
    }

    /// 把上次基准以来的增量攒进 guest 视图，并重记基准。
    pub fn sync(&mut self) {
        if !self.supported {
            return;
        }

        let (aperf, mperf) = read_hw();

        self.aperf = self.aperf.wrapping_add(aperf.wrapping_sub(self.base_aperf));
        self.mperf = self.mperf.wrapping_add(mperf.wrapping_sub(self.base_mperf));
        self.base_aperf = aperf;
        self.base_mperf = mperf;
    }

    /// guest 读某个计数器：先同步再取值。
    pub fn value(&mut self, msr: u32) -> u64 {
        self.sync();

        if msr == x86::msr::IA32_APERF {
            self.aperf
        } else {
            self.mperf
        }
    }

    /// guest 写：只保留写入值，绝不写真实 MSR。
    pub fn set(&mut self, msr: u32, value: u64) {
        if msr == x86::msr::IA32_APERF {
            self.aperf = value;
        } else {
            self.mperf = value;
        }
    }
}

fn read_hw() -> (u64, u64) {
    let mut info = HostExceptionInfo::new();

    let aperf = safe_routine::rdmsr_safe(&mut info, x86::msr::IA32_APERF).unwrap_or(0);
    let mperf = safe_routine::rdmsr_safe(&mut info, x86::msr::IA32_MPERF).unwrap_or(0);

    (aperf, mperf)
}
