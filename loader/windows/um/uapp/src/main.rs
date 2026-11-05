#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui;

use tabs::Tab;

use uapp::{parse_hex_address, parse_process_id, parse_ticks};

mod command;
mod freeze;
mod hypercall;
mod mem_client;
mod mem_test;
mod mem_ui;
mod os_win;
mod tabs;
mod utils;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1180.0, 820.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Test app",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_zoom_factor(1.11);
            Ok(Box::new(MyApp::default()))
        }),
    )
}

/// 原型阶段的后台应答线程：每 10 ms 查一次等待项，全部 ACK `Continue`。
///
/// `HYPERCALL_USER_BREAKPOINT_WAIT_LIST` 只是快照，重复读不会消费等待项，所以这里不需要额外
/// 状态；ACK 本身对同一个 `seq` 也是幂等的。线程自己绑定 `start_processor`
/// （唯一保证虚拟化的核），不跟随 UI 线程的亲和性。
struct WaitWorker {
    stop: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl WaitWorker {
    fn spawn(key: u64, processor: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let worker_stop = Arc::clone(&stop);
        let worker_log = Arc::clone(&log);

        let handle = std::thread::spawn(move || {
            let mut waits = vec![hypercall::HookWait::default(); 8];

            loop {
                if worker_stop.load(Ordering::Relaxed) {
                    break;
                }

                // SAFETY: pinned to a CPU that ran VMXON and using the key from
                // GetInfo; if the hypervisor was stopped first this would #UD,
                // so 停止按钮先 join 本线程再 Stop。
                let Ok(_pinned) = hypercall::PinnedThread::new(processor) else {
                    push_wait_log(
                        &worker_log,
                        format!("绑定 CPU {processor} 失败，自动应答停止"),
                    );
                    break;
                };

                let count = unsafe {
                    hypercall::vmcall(
                        key,
                        hypercall::FUNCTION_USER_BREAKPOINT_WAIT_LIST,
                        waits.as_mut_ptr() as u64,
                        waits.len() as u64,
                    )
                } as usize;
                let count = count.min(waits.len());

                for wait in &waits[..count] {
                    if wait.state != hypercall::WAIT_STATE_PENDING {
                        continue;
                    }

                    let reply = unsafe {
                        hypercall::vmcall3(
                            key,
                            hypercall::FUNCTION_USER_BREAKPOINT_WAIT_REPLY,
                            wait.seq,
                            hypercall::WAIT_ACTION_CONTINUE as u64,
                            hypercall::WAIT_ARG_AUTO,
                        )
                    };

                    push_wait_log(
                        &worker_log,
                        format!(
                            "seq={} cpu={} thread=0x{:X} va=0x{:X} rip=0x{:X} -> {}",
                            wait.seq,
                            wait.cpu,
                            wait.thread,
                            wait.va,
                            wait.rip,
                            if reply == 0 {
                                String::from("Continue")
                            } else {
                                format!("ACK 失败 {reply}")
                            }
                        ),
                    );
                }

                std::thread::sleep(Duration::from_millis(10));
            }
        });

        Self {
            stop,
            log,
            handle: Some(handle),
        }
    }

    /// 取走已记录的行，只把最后几行交给 UI。
    fn drain(&self) -> Vec<String> {
        let mut log = self.log.lock().expect("wait worker log poisoned");
        let keep = log.len().saturating_sub(4);
        let lines = log.split_off(keep);

        log.clear();

        lines
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);

        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn push_wait_log(log: &Arc<Mutex<Vec<String>>>, line: String) {
    let mut log = log.lock().expect("wait worker log poisoned");

    if log.len() >= 256 {
        log.remove(0);
    }

    log.push(line);
}

impl Drop for WaitWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

struct MyApp {
    drv_name: String,
    drv_path: String,
    image_path: String,
    log_path: String,
    device: Option<command::Device>,
    info: Option<command::Info>,
    kernel_args: Option<command::KernelArgs>,
    nonce: u64,
    status: String,
    hook_address: String,
    hook_pid: String,
    hook_kind: u32,
    hook_one_shot: bool,
    wait_timeout: String,
    wait_filter: String,
    set_reg_name: String,
    set_reg_value: String,
    waits: Vec<hypercall::HookWait>,
    /// 最近一次安装用户断点的目标进程；0 表示没有（内核/未安装）。
    breakpoint_pid: u32,
    /// 已经挂起的目标进程线程；`Some` 表示进程处于“整体暂停”。
    frozen: Option<freeze::Frozen>,
    /// 本次等待已经发过应答，不再冻结（否则应答永远等不到命中）。
    ack_pending: bool,
    /// 自动轮询等待项的时间戳（命中后要立刻冻结目标进程）。
    last_wait_poll: Option<std::time::Instant>,
    /// 等待项第一次被看到 Pending 的时间；用于冻结去抖。
    pending_since: Option<std::time::Instant>,
    wait_worker: Option<WaitWorker>,
    wait_log: String,
    self_root: Vec<u8>,
    self_guest: Vec<u8>,
    /// 内存面板（`MEM_*` + uapp OS 层）。
    mem_ui: mem_ui::MemUi,
    /// SVM/NPT hook 页。
    svm_address: String,
    svm_pid: String,
    svm_method: u32,
    /// dr0 hook 的目标 PID 和已见命中数；用于命中后冻结整个进程。
    svm_dr_pid: u32,
    svm_dr_hits: u64,
    /// NtCreateFile 的 NPT 函数 hook：最近一次安装/移除的结果。
    ntcf_installed: bool,
    ntcf_address: u64,
    ntcf_trampoline: u64,
    ntcf_total: u64,
    ntcf_logs: Vec<command::NtCreateFileLog>,
    ntcf_filter: String,
    ntcf_auto: bool,
    ntcf_last_poll: Option<std::time::Instant>,
    /// TSC 选择性隐藏：目标进程 PID 输入。
    tsc_pid: String,
    /// TSC 选择性隐藏：目标模式（0 = 内核、1 = 用户、2 = 内核+用户）。
    tsc_mode: u32,
    /// TSC 选择性隐藏：目标内核模块基址/大小输入。
    tsc_module_base: String,
    tsc_module_size: String,
    /// TSC 选择性隐藏：每次命中退出的补偿 tick 输入。
    tsc_exit_overhead: String,
    /// TSC 选择性隐藏：可见时钟的除数（目标看到 `物理差 / N`）。
    tsc_divisor: String,
    /// TSC 选择性隐藏：是否记录命中日志。
    tsc_hit_log: bool,
    /// TSC 面板状态行。
    tsc_status: String,
    /// 环境页：Start 之前是否下发 WinOffsets。
    push_offsets_on_start: bool,
    /// 环境页：WinOffsets 相关提示。
    win_offsets_status: String,
    tab: Tab,
    /// 上一帧的 tab；切 tab 时把滚动条复位到顶部，避免“按钮在视口上方”。
    last_tab: Tab,
}

impl Default for MyApp {
    fn default() -> Self {
        Self {
            drv_name: String::from("noumen_loader"),
            drv_path: utils::default_driver_path(),
            image_path: utils::default_image_path(),
            log_path: utils::default_log_path(),
            device: None,
            info: None,
            kernel_args: None,
            nonce: 0x1234_5678_9ABC_DEF0,
            status: String::new(),
            hook_address: String::new(),
            hook_pid: String::from("0"),
            hook_kind: command::HOOK_KIND_DETOUR_INTERNAL,
            hook_one_shot: false,
            wait_timeout: String::from("-1"),
            wait_filter: String::new(),
            set_reg_name: String::from("rax"),
            set_reg_value: String::new(),
            waits: Vec::new(),
            breakpoint_pid: 0,
            frozen: None,
            ack_pending: false,
            last_wait_poll: None,
            pending_since: None,
            wait_worker: None,
            wait_log: String::new(),
            self_root: Vec::new(),
            self_guest: Vec::new(),
            mem_ui: mem_ui::MemUi::default(),
            svm_address: String::new(),
            svm_pid: String::from("0"),
            svm_method: command::SvmHookRequest::METHOD_BYTE_PATCH,
            svm_dr_pid: 0,
            svm_dr_hits: 0,
            ntcf_installed: false,
            ntcf_address: 0,
            ntcf_trampoline: 0,
            ntcf_total: 0,
            ntcf_logs: Vec::new(),
            ntcf_filter: String::new(),
            ntcf_auto: false,
            ntcf_last_poll: None,
            tsc_pid: String::from("0"),
            tsc_mode: hypercall::TSC_MODE_BOTH,
            tsc_module_base: String::new(),
            tsc_module_size: String::new(),
            tsc_exit_overhead: String::from("0"),
            tsc_divisor: String::from("10"),
            tsc_hit_log: true,
            tsc_status: String::new(),
            push_offsets_on_start: true,
            win_offsets_status: String::new(),
            tab: Tab::DriverLoad,
            last_tab: Tab::DriverLoad,
        }
    }
}

impl MyApp {
    fn reset_device(&mut self) {
        self.device = None;
        self.info = None;
        self.kernel_args = None;
    }

    fn ensure_device(&mut self) -> Result<&command::Device, String> {
        if self.device.is_none() {
            self.device = Some(command::Device::open().map_err(|err| err.to_string())?);
        }

        Ok(self.device.as_ref().expect("device was just opened"))
    }

    fn request<T>(
        &mut self,
        f: impl FnOnce(&command::Device) -> Result<T, command::Error>,
    ) -> Result<T, String> {
        let device = self.ensure_device()?;
        f(device).map_err(|err| err.to_string())
    }

    fn direct_hypercall(
        &mut self,
        function: u64,
        arg0: u64,
        arg1: u64,
    ) -> Result<u64, String> {
        let info = self.request(|device| device.get_info())?;

        if !info.is_running() {
            return Err(format!("Hypervisor 未运行（{}）", info.state_name()));
        }

        // 始终绑定 start_processor：all-core 和 single-core 模式下它都一定
        // 执行过 VMXON，避免在未虚拟化的 CPU 上 VMCALL 触发 #UD。
        let _pinned = hypercall::PinnedThread::new(info.start_processor)
            .map_err(|err| format!("绑定 CPU {} 失败: {err}", info.start_processor))?;

        Ok(unsafe {
            hypercall::vmcall(info.hypercall_key, function, arg0, arg1)
        })
    }

    /// 与 [`Self::direct_hypercall`] 相同，但多传一个 `R9`（ACK 的 action_arg）。
    fn direct_hypercall3(
        &mut self,
        function: u64,
        arg0: u64,
        arg1: u64,
        arg2: u64,
    ) -> Result<u64, String> {
        let info = self.request(|device| device.get_info())?;

        if !info.is_running() {
            return Err(format!("Hypervisor 未运行（{}）", info.state_name()));
        }

        let _pinned = hypercall::PinnedThread::new(info.start_processor)
            .map_err(|err| format!("绑定 CPU {} 失败: {err}", info.start_processor))?;

        Ok(unsafe {
            hypercall::vmcall3(info.hypercall_key, function, arg0, arg1, arg2)
        })
    }

    fn open_device(&mut self) -> String {
        self.reset_device();

        match self.ensure_device() {
            Ok(_) => format!("已打开 {}", command::DEVICE_PATH),
            Err(err) => err,
        }
    }

    fn ping_driver(&mut self) -> String {
        match self.request(|device| device.ping(0x1234)) {
            Ok(value) => format!("驱动 Ping 成功: 0x{value:08X}"),
            Err(err) => err,
        }
    }

    /// 解析超时输入：空 = 默认，`-1` = 无限，其它是毫秒。
    fn parse_wait_timeout(&self) -> Result<u64, String> {
        let text = self.wait_timeout.trim();

        if text.is_empty() {
            return Ok(0);
        }

        if text == "-1" {
            return Ok(hypercall::INFINITE);
        }

        text.parse::<u64>()
            .map_err(|_| format!("无效超时（毫秒，-1 表示无限）: {text}"))
    }

    /// 设置用户断点的等待超时。等待本身由 `USER_BREAKPOINT` 类型决定，
    /// 不需要开关。
    fn set_wait_timeout(&mut self) -> String {
        let timeout = match self.parse_wait_timeout() {
            Ok(timeout) => timeout,
            Err(err) => return err,
        };

        match self.direct_hypercall(hypercall::FUNCTION_USER_BREAKPOINT_WAIT_TIMEOUT, timeout, 0) {
            Ok(0) => {
                if timeout == hypercall::INFINITE {
                    String::from("等待超时已设为无限：只由应答/移除断点/owner 变化放行")
                } else if timeout == 0 {
                    String::from("等待超时已恢复默认（1000 ms）")
                } else {
                    format!("等待超时已设为 {timeout} ms")
                }
            }
            Ok(error) => format!("设置等待超时失败: 状态 {error}"),
            Err(err) => err,
        }
    }

    /// 解析线程过滤输入：空 / `0` 表示清除。
    fn parse_wait_filter(&self) -> Result<u64, String> {
        let text = self.wait_filter.trim();

        if text.is_empty() || text == "0" || text.eq_ignore_ascii_case("any") {
            return Ok(0);
        }

        parse_hex_address(text)
            .map_err(|err| format!("无效 ETHREAD（十六进制）: {err}"))
    }

    /// 设置当前地址的线程过滤（只等待指定 ETHREAD 的命中）。
    fn set_wait_filter(&mut self) -> String {
        let address = match parse_hex_address(&self.hook_address) {
            Ok(address) => address,
            Err(err) => return format!("线程过滤需要目标地址: {err}"),
        };
        let thread = match self.parse_wait_filter() {
            Ok(thread) => thread,
            Err(err) => return err,
        };

        let message = match self.direct_hypercall(
            hypercall::FUNCTION_USER_BREAKPOINT_WAIT_FILTER,
            address,
            thread,
        ) {
            Ok(0) if thread == 0 => format!("已清除 0x{address:X} 的线程过滤"),
            Ok(0) => format!("0x{address:X} 只等待线程 ETHREAD=0x{thread:X}"),
            Ok(error) => format!("设置线程过滤失败: 状态 {error}"),
            Err(err) => err,
        };

        // 设置过滤会取消该地址上已有的等待项，刷新一下本地缓存。
        let _ = self.poll_waits(None);
        message
    }

    /// 改最新等待项里的一个通用寄存器。
    fn set_wait_reg(&mut self) -> String {
        let Some(wait) = self.waits.last().copied() else {
            return String::from("没有等待项：先点“读取等待项”");
        };

        let Some(index) = hypercall::gpr_index(&self.set_reg_name) else {
            return format!(
                "无效寄存器: {}（可用 rax..r15 或 0..15）",
                self.set_reg_name.trim()
            );
        };

        let value = match parse_hex_address(&self.set_reg_value) {
            Ok(value) => value,
            Err(err) => return format!("无效寄存器值: {err}"),
        };

        let message = match self.direct_hypercall3(
            hypercall::FUNCTION_USER_BREAKPOINT_WAIT_SET_REG,
            wait.seq,
            index as u64,
            value,
        ) {
            Ok(0) => format!(
                "seq={} 的 {} 已改为 0x{:X}（Continue/Skip 时写回目标线程）",
                wait.seq,
                hypercall::GPR_NAMES[index as usize],
                value
            ),
            Ok(status) => format!("改写寄存器失败: 状态 {status}"),
            Err(err) => err,
        };

        // 刷新缓存，UI 立刻显示新值。
        let _ = self.poll_waits(None);
        message
    }

    /// 取消等待项；`seq == 0` 取消全部。
    ///
    /// 应答只在目标线程下一次 `#BP` 生效。如果线程已经离开断点（停在
    /// `system("pause")`、控制台 I/O、被挂起），应答会一直留在槽位里，那个
    /// 槽位之后的命中会被 `AlreadyPending` 拒绝，断点看起来“不再等待”。
    fn cancel_wait(&mut self, seq: u64) -> String {
        let message = match self.direct_hypercall(
            hypercall::FUNCTION_USER_BREAKPOINT_WAIT_CANCEL,
            seq,
            0,
        ) {
            Ok(count) => {
                if count == 0 {
                    String::from("没有可取消的等待项")
                } else if seq == 0 {
                    format!("已取消全部 {count} 个等待项")
                } else {
                    format!("已取消等待项 seq={seq}")
                }
            }
            Err(err) => err,
        };

        let _ = self.poll_waits(None);
        message
    }

    /// 单步一条指令，并把目标停下来的新位置显示出来。
    ///
    /// hypervisor 的做法：MTF 执行完当前指令后，在“下一条指令”（MTF 退出时的
    /// guest RIP）处补一个一次性断点；目标再次命中时仍是一个普通的 `等待应答`
    /// 等待项，`rip`/`va` 就是下一行。
    fn single_step(&mut self) -> String {
        let _ = self.poll_waits(None);

        let Some(wait) = self.waits.last().copied() else {
            return String::from("没有等待项：先让目标命中用户断点");
        };

        if wait.state != hypercall::WAIT_STATE_PENDING {
            return format!(
                "当前不是待应答状态（{}）",
                hypercall::wait_state_name(wait.state)
            );
        }

        match self.direct_hypercall3(
            hypercall::FUNCTION_USER_BREAKPOINT_WAIT_REPLY,
            wait.seq,
            hypercall::WAIT_ACTION_SINGLE_STEP as u64,
            0,
        ) {
            Ok(0) => {}
            Ok(status) => return format!("单步应答失败: 状态 {status}"),
            Err(err) => return err,
        }

        // 只执行一条指令，随即停在下一条；等一下再读，把新位置显示出来。
        std::thread::sleep(std::time::Duration::from_millis(5));
        let _ = self.poll_waits(None);

        match self.waits.last().copied() {
            Some(wait) if wait.state == hypercall::WAIT_STATE_PENDING => format!(
                "已单步：停在 0x{:X}（seq={}，等待应答）",
                wait.rip, wait.seq
            ),
            Some(wait) => format!(
                "已单步：seq={} 状态 {}",
                wait.seq,
                hypercall::wait_state_name(wait.state)
            ),
            None => String::from("已单步：目标没有停在下一条指令（可能跨页，退化为继续）"),
        }
    }

    /// 读取等待项；`action` 为 `Some` 时对每条 Pending 项按该动作 ACK。
    fn poll_waits(&mut self, action: Option<u32>) -> String {
        let mut waits = vec![hypercall::HookWait::default(); 16];

        let count = match self.direct_hypercall(
            hypercall::FUNCTION_USER_BREAKPOINT_WAIT_LIST,
            waits.as_mut_ptr() as u64,
            waits.len() as u64,
        ) {
            Ok(count) => (count as usize).min(waits.len()),
            Err(err) => return err,
        };

        waits.truncate(count);

        // ABI 自检：`HookWait` 的布局变过几次（120 → 128 → 208 → 216 字节）。
        // 如果 uapp 和 hypervisor 不是同一次构建，`state` 会读到别的字段，下面的
        // ACK 会被静默跳过，表现成“点了 Continue 没反应、只有超时在放行”。
        let abi_mismatch = waits.iter().any(|wait| {
            !matches!(
                wait.state,
                hypercall::WAIT_STATE_IDLE
                    | hypercall::WAIT_STATE_PENDING
                    | hypercall::WAIT_STATE_APPLYING
            ) || wait.tsc == 0
                || wait.hits == 0
                || wait.slot as usize >= 32
                || wait.va == 0
        });

        let mut acked = 0;
        let mut failed = Vec::new();

        if let Some(action) = action {
            for wait in &waits {
                if wait.state != hypercall::WAIT_STATE_PENDING {
                    continue;
                }

                match self.direct_hypercall3(
                    hypercall::FUNCTION_USER_BREAKPOINT_WAIT_REPLY,
                    wait.seq,
                    action as u64,
                    0,
                ) {
                    Ok(0) => acked += 1,
                    Ok(status) => failed.push(format!("seq={} 状态 {status}", wait.seq)),
                    Err(err) => failed.push(err),
                }
            }
        }

        let summary = waits
            .last()
            .map(|wait| {
                format!(
                    "最新 seq={} cpu={} thread=0x{:X} cr3=0x{:X} va=0x{:X} rip=0x{:X} rsp=0x{:X} {}",
                    wait.seq,
                    wait.cpu,
                    wait.thread,
                    wait.cr3,
                    wait.va,
                    wait.rip,
                    wait.rsp,
                    hypercall::wait_state_name(wait.state)
                )
            })
            .unwrap_or_else(|| String::from("没有等待项"));

        self.waits = waits;

        if acked > 0 {
            self.ack_pending = true;
        }

        self.sync_target_freeze();

        let mut message = format!("读取 {} 条断点等待；{summary}", self.waits.len());

        if abi_mismatch {
            message.push_str(
                "；【警告】等待项字段异常（state/tsc/slot/va 不合理）：uapp 与 noumen \
                 很可能不是同一次构建，ACK 会被跳过。请重新构建并重新加载 noumen 镜像",
            );
        }

        if let Some(action) = action {
            message.push_str(&format!(
                "；ACK {} {acked} 条",
                if action == hypercall::WAIT_ACTION_SINGLE_STEP {
                    "SingleStep"
                } else {
                    "Continue"
                }
            ));
        }

        if !failed.is_empty() {
            message.push_str(&format!("；失败: {}", failed.join(", ")));
        }

        message
    }

    /// 启动后台应答线程（原型阶段的 uapp 侧）。
    fn start_wait_worker(&mut self) -> String {
        if self.wait_worker.is_some() {
            return String::from("自动应答已经在运行");
        }

        let info = match self.request(|device| device.get_info()) {
            Ok(info) => info,
            Err(err) => return err,
        };

        if !info.is_running() {
            return format!("Hypervisor 未运行（{}）", info.state_name());
        }

        if info.hypercall_key == 0 {
            return String::from("HypercallKey 为 0，hypercall 已禁用");
        }

        self.wait_worker = Some(WaitWorker::spawn(info.hypercall_key, info.start_processor));

        format!(
            "自动应答已启动：CPU {} 每 10 ms 轮询等待项并 ACK Continue",
            info.start_processor
        )
    }

    fn stop_wait_worker(&mut self) -> String {
        match self.wait_worker.take() {
            Some(mut worker) => {
                worker.stop();
                String::from("自动应答已停止")
            }
            None => String::from("自动应答没有运行"),
        }
    }

    /// TSC 选择性隐藏：下发目标进程 PID + 目标模式（`0` = 清除）。
    fn tsc_set_pid(&mut self) -> String {
        let pid = match parse_process_id(&self.tsc_pid) {
            Ok(pid) => pid as u64,
            Err(err) => {
                let message = format!("PID 解析失败: {err}");

                self.tsc_status = message.clone();
                return message;
            }
        };

        let mode = u64::from(self.tsc_mode);
        let mode_name = hypercall::tsc_mode_name(self.tsc_mode);

        let message = match self.direct_hypercall(
            hypercall::FUNCTION_TSC_SET_TARGET_PID,
            pid,
            mode,
        ) {
            Ok(0) if pid == 0 => String::from("TSC: 已清除目标进程"),
            Ok(0) => format!("TSC: 目标进程 PID = {pid}，模式 = {mode_name}"),
            Ok(1) => String::from("TSC: 目标模式非法（只能是 0/1/2）"),
            Ok(other) => format!("TSC: 设置进程失败，RAX = {other:#x}"),
            Err(err) => format!("TSC: {err}"),
        };

        self.tsc_status = message.clone();
        message
    }

    /// TSC 选择性隐藏：下发目标内核模块范围（大小 0 或空输入 = 清除）。
    fn tsc_set_module(&mut self) -> String {
        let base = if self.tsc_module_base.trim().is_empty() {
            0
        } else {
            match parse_hex_address(&self.tsc_module_base) {
                Ok(value) => value,
                Err(err) => {
                    let message = format!("模块基址解析失败: {err}");

                    self.tsc_status = message.clone();
                    return message;
                }
            }
        };

        let size = if self.tsc_module_size.trim().is_empty() {
            0
        } else {
            match parse_hex_address(&self.tsc_module_size) {
                Ok(value) => value,
                Err(err) => {
                    let message = format!("模块大小解析失败: {err}");

                    self.tsc_status = message.clone();
                    return message;
                }
            }
        };

        if size == 0 {
            self.tsc_module_base.clear();
            self.tsc_module_size.clear();
        }

        let message = match self.direct_hypercall3(
            hypercall::FUNCTION_TSC_SET_TARGET_MODULE,
            0,
            base,
            size,
        ) {
            Ok(0) if size == 0 => String::from("TSC: 已清除目标模块"),
            Ok(0) => format!("TSC: 目标模块 {base:#x} + {size:#x}"),
            Ok(1) => String::from("TSC: 模块范围非法（必须在内核半区且 ≤ 256 MiB）"),
            Ok(other) => format!("TSC: 设置模块失败，RAX = {other:#x}"),
            Err(err) => format!("TSC: {err}"),
        };

        self.tsc_status = message.clone();
        message
    }

    /// TSC 选择性隐藏：开关命中日志。
    fn tsc_set_hit_log(&mut self, enabled: bool) -> String {
        let message = match self.direct_hypercall(
            hypercall::FUNCTION_TSC_SET_HIT_LOG,
            u64::from(enabled),
            0,
        ) {
            Ok(0) => format!("TSC: 命中日志已{}", if enabled { "开启" } else { "关闭" }),
            Ok(code) => format!("TSC: 命中日志设置失败 code={code}"),
            Err(err) => format!("TSC: 命中日志设置失败 {err}"),
        };

        self.tsc_status = message.clone();

        message
    }

    /// TSC 选择性隐藏：下发可见时钟的除数（目标看到 `物理差 / N`）。
    fn tsc_set_divisor(&mut self) -> String {
        let divisor = match parse_ticks(&self.tsc_divisor) {
            Ok(divisor) => divisor,
            Err(err) => {
                let message = format!("TSC 除数解析失败: {err}");

                self.tsc_status = message.clone();
                return message;
            }
        };

        let message = match self.direct_hypercall(
            hypercall::FUNCTION_TSC_SET_DIVISOR,
            divisor,
            0,
        ) {
            Ok(0) => format!("TSC: 除数 = {divisor}（可见时钟每 {divisor} tick 走 1）"),
            Ok(code) => format!("TSC: 除数设置失败 code={code}"),
            Err(err) => format!("TSC: 除数设置失败 {err}"),
        };

        self.tsc_status = message.clone();

        message
    }

    /// TSC 选择性隐藏：下发每次命中退出的补偿 tick（`0` = 不补偿）。
    ///
    /// 账本测不到 guest→root 转换本身，所以每次退出会漏一截。目标进程量到的
    /// `rdtsc; cpuid; rdtsc` 差值里夹着两次退出，补偿值取「实测残差 / 2」。
    fn tsc_set_exit_overhead(&mut self) -> String {
        let ticks = match parse_ticks(&self.tsc_exit_overhead) {
            Ok(ticks) => ticks,
            Err(err) => {
                let message = format!("TSC 补偿值解析失败: {err}");

                self.tsc_status = message.clone();
                return message;
            }
        };

        let message = match self.direct_hypercall(
            hypercall::FUNCTION_TSC_SET_EXIT_OVERHEAD,
            ticks,
            0,
        ) {
            Ok(0) if ticks == 0 => String::from("TSC: 已关闭退出补偿"),
            Ok(0) => format!("TSC: 每次命中退出补偿 {ticks} tick"),
            Ok(1) => String::from("TSC: 补偿值超范围（上限 1000000 tick）"),
            Ok(other) => format!("TSC: 设置补偿失败，RAX = {other:#x}"),
            Err(err) => format!("TSC: {err}"),
        };

        self.tsc_status = message.clone();
        message
    }

    fn hide_self(&mut self) -> String {
        match self.direct_hypercall(hypercall::FUNCTION_HIDE_SELF, 0, 0) {
            Ok(count) if count < u32::MAX as u64 => {
                format!("HideSelf: 本核隐藏 {count} 页（其它核下次 VM exit 生效）")
            }
            Ok(_) => String::from("HideSelf 失败：没有 PFN 列表"),
            Err(err) => err,
        }
    }

    fn read_self_pages(&mut self) -> String {
        let mut root = vec![0u8; 4096];
        let root_result = self.direct_hypercall(
            hypercall::FUNCTION_READ_SELF,
            0,
            root.as_mut_ptr() as u64,
        );
        let guest_result = self.request(|device| device.read_kernel_image(0, 4096));

        match (root_result, guest_result) {
            (Ok(root_bytes), Ok(guest))
                if root_bytes == 4096 && guest.len() == 4096 =>
            {
                let same = root == guest;
                let root_head = root
                    .iter()
                    .take(16)
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let guest_head = guest
                    .iter()
                    .take(16)
                    .map(|byte| format!("{byte:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ");

                self.self_root = root;
                self.self_guest = guest;

                format!(
                    "镜像页 0：root 前16字节 [{root_head}]，guest(KernelBase) 前16字节 [{guest_head}]，{}",
                    if same { "相同（未隐藏）" } else { "不同（HideSelf 生效）" }
                )
            }
            (Ok(0), _) => String::from("root 读取失败：没有 PFN 列表"),
            (Err(err), _) => err,
            (_, Err(err)) => format!("guest 读取 KernelBase 失败: {err}"),
            _ => String::from("读取自身内存失败"),
        }
    }

    fn resolve_nt_create_file(&mut self) -> String {
        let address = match self.request(|device| device.resolve_nt_create_file()) {
            Ok(address) => address,
            Err(err) => return format!("解析 NtCreateFile 失败: {err}"),
        };

        self.hook_address = format!("0x{address:X}");
        self.hook_pid = String::from("0");

        format!(
            "NtCreateFile=0x{address:X}；地址已填入，选择类型后点“安装 EPT hook”"
        )
    }

    fn ept_ntcf_hook(&mut self, direct: bool, remove: bool) -> String {
        let address = match self.request(|device| device.resolve_nt_create_file()) {
            Ok(address) => address,
            Err(err) => return format!("解析 NtCreateFile 失败: {err}"),
        };

        self.hook_address = format!("0x{address:X}");
        self.hook_pid = String::from("0");
        self.hook_kind = if direct {
            command::HOOK_KIND_DETOUR_TRAMPOLINE
        } else {
            command::HOOK_KIND_DETOUR_INTERNAL
        };

        self.ept_hook(remove)
    }

    fn refresh_info(&mut self) -> String {
        match self.request(|device| device.get_info()) {
            Ok(info) => {
                let message = format!(
                    "协议 {}, 模式 {}, 状态 {}, 核心 {}, 启动核心 {}, HypercallKey 0x{:X}",
                    info.protocol_version,
                    info.mode_name(),
                    info.state_name(),
                    info.cpu_count,
                    info.start_processor,
                    info.hypercall_key
                );
                self.info = Some(info);
                message
            }
            Err(err) => err,
        }
    }

    fn set_image_path(&mut self) -> String {
        let path = Path::new(self.image_path.trim()).to_path_buf();

        match self.request(|device| device.set_image_path(&path)) {
            Ok(()) => format!("镜像路径已设置: {}", path.display()),
            Err(err) => err,
        }
    }

    fn set_log_path(&mut self) -> String {
        let path = Path::new(self.log_path.trim()).to_path_buf();

        match self.request(|device| device.set_log_path(&path)) {
            Ok(()) => format!("日志路径已设置: {}", path.display()),
            Err(err) => err,
        }
    }

    fn refresh_kernel_args(&mut self) -> String {
        match self.request(|device| device.get_kernel_args()) {
            Ok(args) => {
                let message = format!(
                    "KernelArgs: CR3 0x{:X}, 内核 0x{:X}/0x{:X}, 核心 {}",
                    args.system_cr3, args.kernel_base, args.kernel_size, args.cpu_count
                );
                self.kernel_args = Some(args);
                message
            }
            Err(err) => err,
        }
    }

    fn start_hypervisor(&mut self) -> String {
        self.start_hypervisor_with(false)
    }

    fn start_hypervisor_single_core(&mut self) -> String {
        self.start_hypervisor_with(true)
    }

    /// 按当前 OS build 选一张偏移表；选不到返回全 0（`build == 0`）。
    fn selected_win_offsets(&mut self) -> Result<command::WinOffsets, String> {
        let os = self.request(|device| device.get_os_info())?;

        Ok(match uapp::win_offsets::select(os.build) {
            Some(table) => command::WinOffsets::from_table(table),
            None => command::WinOffsets::default(),
        })
    }

    /// 只刷新面板上的“当前会选哪张表”，不下发。
    fn refresh_win_offsets(&mut self) -> String {
        let os = match self.request(|device| device.get_os_info()) {
            Ok(os) => os,
            Err(err) => return err,
        };

        let message = match uapp::win_offsets::select(os.build) {
            Some(table) => format!(
                "OS build {} -> 偏移表 build {}（pid=0x{:X}, name=0x{:X}, kthread=0x{:X}）",
                os.build, table.build, table.eproc_pid, table.eproc_name, table.kthread_process
            ),
            None => format!(
                "OS build {} 没有对应偏移表（表里最新 {}，用 tools/gen_win_offsets.py 补）",
                os.build,
                uapp::win_offsets::newest_build()
            ),
        };

        self.win_offsets_status = message.clone();
        message
    }

    /// 把按 build 选好的 Windows 结构偏移表下发给驱动；`Start` 之前调用。
    ///
    /// 选不到表（未知 build）时下发 `build = 0`，hypervisor 侧据此关掉依赖偏移的
    /// 进程检查（TSC 选择性隐藏按 PID 的那条路径会一直不匹配）。
    fn push_win_offsets(&mut self) -> Result<Option<u32>, String> {
        let offsets = self.selected_win_offsets()?;
        let build = if offsets.build == 0 {
            None
        } else {
            Some(offsets.build)
        };

        self.request(|device| device.set_win_offsets(&offsets))?;
        self.win_offsets_status = match build {
            Some(build) => format!("Start 前已下发偏移表 build {build}"),
            None => String::from("Start 前未找到匹配偏移表（下发 build = 0）"),
        };

        Ok(build)
    }

    /// 运行时用 hypercall（`0x203`）覆盖 hypervisor 手里的偏移表。
    fn hypercall_set_win_offsets(&mut self) -> String {
        let offsets = match self.selected_win_offsets() {
            Ok(offsets) => offsets,
            Err(err) => return err,
        };

        if offsets.build == 0 {
            let message = String::from("没有匹配的偏移表，未下发");
            self.win_offsets_status = message.clone();
            return message;
        }

        let size = std::mem::size_of::<command::WinOffsets>() as u64;
        let pointer = &offsets as *const command::WinOffsets as u64;

        let message = match self.direct_hypercall(
            hypercall::FUNCTION_TSC_SET_WIN_OFFSETS,
            pointer,
            size,
        ) {
            Ok(0) => format!(
                "Hypercall 下发偏移表 build {}（hypervisor 侧已生效）",
                offsets.build
            ),
            Ok(other) => format!("Hypercall 下发失败，RAX = {other:#x}"),
            Err(err) => format!("Hypercall 下发失败: {err}"),
        };

        self.win_offsets_status = message.clone();
        message
    }

    fn start_hypervisor_with(&mut self, single_core: bool) -> String {
        // 偏移表必须在 `_start` 之前下发：hypervisor 只在启动时读一次 KernelArgs。
        let offsets = if self.push_offsets_on_start {
            match self.push_win_offsets() {
                Ok(build) => build,
                Err(err) => {
                    return format!("下发 WinOffsets 失败: {err}");
                }
            }
        } else {
            self.win_offsets_status = String::from("按选项跳过 WinOffsets（KernelArgs 里 build = 0）");
            None
        };

        let offsets_note = match (self.push_offsets_on_start, offsets) {
            (false, _) => String::from("，未下发偏移表"),
            (true, Some(build)) => format!("，偏移表 build {build}"),
            (true, None) => String::from("，未找到匹配的偏移表（进程检查不可用）"),
        };

        match self.request(|device| device.start_hypervisor(single_core, false)) {
            Ok(()) => {
                let mode = if single_core {
                    "单核"
                } else {
                    "全部核心"
                };
                format!("启动成功（{mode}{offsets_note}）；{}", self.refresh_info())
            }
            Err(err) => err,
        }
    }

    fn stop_hypervisor(&mut self) -> String {
        // 先停后台应答线程：它绑定在虚拟化过的核上，Stop 之后 VMCALL 会 #UD。
        self.stop_wait_worker();

        match self.request(|device| device.stop_hypervisor()) {
            Ok(()) => {
                // Drop the Running snapshot before refreshing: a stale one
                // would let the direct VMCALL run on a devirtualized CPU.
                self.info = None;
                format!("Hypervisor 已停止；{}", self.refresh_info())
            }
            Err(err) => err,
        }
    }

    fn next_nonce(&mut self) -> u64 {
        let nonce = self.nonce;
        self.nonce = self.nonce.wrapping_add(1);
        nonce
    }

    /// 调试器语义：等待项出现时挂起目标进程的**所有**线程，应答/放行后恢复。
    ///
    /// 我们的断点只让命中线程在 guest 里自旋 `INT3`，进程里其它线程会继续跑
    /// （控制台还在输出）。要“整个进程停住”，只能由 uapp 把目标线程全部
    /// `SuspendThread`。命中的线程也一起挂起：它停在 `INT3` 上，恢复后重新
    /// 执行 `0xCC` 会再次命中，应答就在那一刻应用。
    fn sync_target_freeze(&mut self) {
        let waiting = self
            .waits
            .iter()
            .any(|wait| wait.state == hypercall::WAIT_STATE_PENDING);

        // 自动应答开着时，每次命中都会被立刻 ACK（微秒级），冻结/恢复只会变成
        // 无意义的抖动（每命中一次挂起 4 个线程再恢复）。这种情况直接跳过，
        // 把“进程整体停住”留给真正会停留的等待。
        let auto = self.wait_worker.is_some();

        if waiting && !self.ack_pending && !auto {
            // 去抖：等待项要持续一小段时间才冻结，避免“命中即继续”时来回挂起。
            let since = *self.pending_since.get_or_insert_with(std::time::Instant::now);

            if since.elapsed() >= Duration::from_millis(200)
                && self.frozen.is_none()
                && self.breakpoint_pid != 0
            {
                match freeze::freeze(self.breakpoint_pid) {
                    Ok(frozen) => {
                        self.wait_log.push_str(&format!(
                            "已挂起目标进程 pid={} 的 {} 个线程（进程整体停住）\n",
                            frozen.pid(),
                            frozen.count()
                        ));
                        self.frozen = Some(frozen);
                    }
                    Err(error) => {
                        self.wait_log.push_str(&format!("挂起目标进程失败: {error}\n"));
                    }
                }
            }
        } else {
            self.pending_since = None;

            if let Some(frozen) = self.frozen.take() {
                let resumed = frozen.thaw();
                self.wait_log
                    .push_str(&format!("已恢复 {resumed} 个目标线程\n"));
            }
        }

        if !self.waits.iter().any(|wait| {
            matches!(
                wait.state,
                hypercall::WAIT_STATE_PENDING | hypercall::WAIT_STATE_APPLYING
            )
        }) {
            self.ack_pending = false;
            self.pending_since = None;
        }
    }

    fn thaw_target(&mut self) {
        if let Some(frozen) = self.frozen.take() {
            let resumed = frozen.thaw();
            self.wait_log
                .push_str(&format!("已恢复 {resumed} 个目标线程\n"));
        }

        self.ack_pending = false;
    }

    /// 装了用户断点就自动轮询等待项：命中后必须立刻挂起目标进程，不能等用户
    /// 点“读取等待项”。
    fn auto_poll_waits(&mut self, ctx: &egui::Context) {
        let running = self.info.as_ref().is_some_and(|info| info.is_running());

        if !running {
            return;
        }

        let now = std::time::Instant::now();
        let due = self
            .last_wait_poll
            .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(150));

        if due {
            self.last_wait_poll = Some(now);

            // DR0 has no wait entry: poll the hit counter and freeze the whole
            // target process on the first hit (only the hitting thread is
            // parked by #DB in the hypervisor).
            if self.svm_dr_pid != 0 {
                if let Ok(hits) = self.direct_hypercall(
                    hypercall::FUNCTION_SVM_DR_STATUS,
                    0,
                    0,
                ) {
                    if hits != self.svm_dr_hits {
                        self.svm_dr_hits = hits;

                        if self.frozen.is_none() {
                            match freeze::freeze(self.svm_dr_pid) {
                                Ok(frozen) => self.frozen = Some(frozen),
                                Err(error) => {
                                    self.status = format!(
                                        "DR0 已命中（hits={hits}），但冻结进程 {} 失败: {error}",
                                        self.svm_dr_pid
                                    );
                                }
                            }
                        }
                    }
                }
            }

            if self.breakpoint_pid != 0 {
                let _ = self.poll_waits(None);
            }
        }

        if self.breakpoint_pid != 0 || self.svm_dr_pid != 0 {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    /// 通过 loader 安装或移除配置的 EPT hook。
    ///
    /// 内核地址使用 `pid = 0`。r3 地址则由 loader attach 到 `pid`、换入目标页、
    /// 用 MDL 锁住，并在 `VMCALL` 期间保持 attach，于是 hypervisor 用目标进程
    /// 的 CR3 解析，页框号保持稳定。
    fn svm_hook(&mut self, remove: bool) -> String {
        let address = match parse_hex_address(&self.svm_address) {
            Ok(address) => address,
            Err(error) => return error,
        };

        let pid = match parse_process_id(&self.svm_pid) {
            Ok(pid) => pid,
            Err(error) => return error,
        };

        let info = match self.request(|device| device.get_info()) {
            Ok(info) => {
                self.info = Some(info);
                info
            }
            Err(error) => return error,
        };

        if !info.is_running() {
            return format!("Hypervisor 未运行（{}）", info.state_name());
        }

        let method = self.svm_method;
        let kind = command::HOOK_KIND_BREAKPOINT;

        if !remove
            && method == command::SvmHookRequest::METHOD_DR_BREAKPOINT
            && pid == 0
        {
            return String::from(
                "dr0 必须填目标进程 PID：uapp 命中后要靠它冻结整个进程",
            );
        }

        if !remove
            && method == command::SvmHookRequest::METHOD_BYTE_PATCH
            && pid == 0
        {
            return String::from(
                "SVM byte-patch 必须填目标进程 PID：否则无法锁页，\
                 hypervisor 只能用 VMCALL 时刻的 CR3 翻译，用户地址会返回 2",
            );
        }
        let response = match self.request(|device| {
            device.svm_hook(address, method, kind, remove, command::PROCESSOR_ANY, pid)
        }) {
            Ok(response) => response,
            Err(error) => return error,
        };

        if response.reply == 0 {
            if !remove && method == command::SvmHookRequest::METHOD_DR_BREAKPOINT {
                self.svm_dr_pid = pid;
                // The counter is global and survives previous hooks; take a
                // baseline now so the first poll only reacts to new hits.
                self.svm_dr_hits = self
                    .direct_hypercall(hypercall::FUNCTION_SVM_DR_STATUS, 0, 0)
                    .unwrap_or(0);
            } else if remove && method == command::SvmHookRequest::METHOD_DR_BREAKPOINT {
                self.svm_dr_pid = 0;

                if let Some(frozen) = self.frozen.take() {
                    frozen.thaw();
                }
            } else if method == command::SvmHookRequest::METHOD_BYTE_PATCH {
                self.breakpoint_pid = if remove { 0 } else { pid };
            }
        }

        format!(
            "SVM {} {} va=0x{:X} method={} kind={} -> reply={} ({}) cpu={}",
            if remove { "remove" } else { "install" },
            if remove { "" } else { "hook" },
            address,
            command::SvmHookRequest::method_name(method),
            kind,
            response.reply,
            command::hook_status_name(response.reply),
            response.processor
        )
    }

    fn ntcf_hook(&mut self, remove: bool) -> String {
        let action = if remove {
            command::NTCF_ACTION_REMOVE
        } else {
            command::NTCF_ACTION_INSTALL
        };

        let result = match self.ensure_device() {
            Ok(device) => device.nt_create_file_hook(action, command::PROCESSOR_ANY),
            Err(err) => {
                return format!(
                    "{} NtCreateFile hook 失败: {err}",
                    if remove { "移除" } else { "安装" }
                )
            }
        };

        match result {
            Ok(response) => {
                self.ntcf_address = response.address;
                self.ntcf_trampoline = response.trampoline;
                self.ntcf_installed = response.installed != 0;

                if response.reply != 0 {
                    return format!(
                        "{} NtCreateFile hook 失败: 状态 {} - {}（NtCreateFile 0x{:016X}，\
                         trampoline 0x{:016X}）",
                        if remove { "移除" } else { "安装" },
                        response.reply,
                        command::hook_status_name(response.reply),
                        response.address,
                        response.trampoline
                    );
                }

                if remove {
                    return format!(
                        "已移除 NtCreateFile hook: 0x{:016X}（trampoline 0x{:016X} 保留复用）",
                        response.address, response.trampoline
                    );
                }

                // 安装成功后立刻拉一次日志：确认 handler 真的被调用了，
                // 而不是只在 hypervisor 里装了个空壳。
                let probe = self.ntcf_refresh_log();

                let probe = match probe {
                    Ok((total, count)) => {
                        format!("；当前已命中 {total} 次，缓存 {count} 条")
                    }
                    Err(err) => format!("；读日志失败: {err}"),
                };

                format!(
                    "已安装 NtCreateFile hook: NtCreateFile 0x{:016X}，handler 0x{:016X}，\
                     trampoline 0x{:016X}（CPU {}{probe}）",
                    response.address, response.handler, response.trampoline, response.processor
                )
            }
            Err(err) => match err {
                command::Error::Command {
                    status: command::STATUS_INVALID_DEVICE_STATE,
                    ..
                } => String::from("已经安装过 NtCreateFile hook：先点「移除 Hook」再安装"),
                command::Error::Command {
                    status: command::STATUS_NOT_FOUND,
                    ..
                } => String::from(
                    "当前没有安装 NtCreateFile hook（Hypervisor Stop 时会被一起移除）",
                ),
                other => format!(
                    "{} NtCreateFile hook 失败: {other}",
                    if remove { "移除" } else { "安装" }
                ),
            },
        }
    }

    /// 拉一次命中日志快照，返回（累计命中数, 本次条数）。
    fn ntcf_refresh_log(&mut self) -> Result<(u64, usize), String> {
        let (response, logs) = self.request(|device| {
            device.read_nt_create_file_log(command::NTCF_LOG_CAPACITY as u32)
        })?;

        self.ntcf_address = response.address;
        self.ntcf_trampoline = response.trampoline;
        self.ntcf_total = response.total_records;
        self.ntcf_installed = response.installed != 0;
        self.ntcf_logs = logs;

        Ok((self.ntcf_total, self.ntcf_logs.len()))
    }

    fn ntcf_read_log(&mut self) -> String {
        match self.ntcf_refresh_log() {
            Ok((total, count)) => format!(
                "NtCreateFile 日志: {count} 条（累计 {total} 次命中，环形缓冲 \
                 {} 条）",
                command::NTCF_LOG_CAPACITY
            ),
            Err(err) => format!("读取 NtCreateFile 日志失败: {err}"),
        }
    }

    fn ept_hook(&mut self, remove: bool) -> String {
        let address = match parse_hex_address(&self.hook_address) {
            Ok(address) => address,
            Err(error) => return error,
        };

        let pid = match parse_process_id(&self.hook_pid) {
            Ok(pid) => pid,
            Err(error) => return error,
        };

        // 重新读取而不是相信缓存的快照：Stop 之后缓存可能仍显示 Running。
        let info = match self.request(|device| device.get_info()) {
            Ok(info) => {
                self.info = Some(info);
                info
            }
            Err(error) => return error,
        };

        if !info.is_running() {
            return format!("Hypervisor 未运行（{}）", info.state_name());
        }

        let kind = self.hook_kind;

        // r3 断点/隐藏页必须指定目标进程：`pid = 0` 时驱动不会 attach，
        // hypervisor 会用**调用者（uapp）自己的 CR3** 翻译地址，断点就装到了
        // uapp 的代码页上——被调试进程执行同一段代码不会命中，表现就是
        // “装了断点，被调试进程照跑”。
        if !remove && pid == 0 && matches!(kind, command::HOOK_KIND_USER_BREAKPOINT | command::HOOK_KIND_HIDE) {
            return String::from(
                "r3 地址必须填目标进程 PID：填 0 会把断点装到 uapp 自己的地址空间，\
                 被调试进程不会命中",
            );
        }

        let kind_name = match kind {
            command::HOOK_KIND_DETOUR_INTERNAL => "EPT 钩子",
            command::HOOK_KIND_DETOUR_TRAMPOLINE => "NtCreateFile proxy 钩子",
            command::HOOK_KIND_HIDE => "隐藏页",
            command::HOOK_KIND_USER_BREAKPOINT => "用户断点",
            _ => "EPT 断点",
        };
        let action = if remove { "移除" } else { "安装" };

        match self.request(|device| {
            device.ept_hook(
                address,
                kind,
                remove,
                command::PROCESSOR_ANY,
                pid,
            )
        }) {
            Ok(response) => {
                if response.reply != 0 {
                    return format!(
                        "{action}{kind_name}失败 (0x{address:016X}, pid {pid}, CPU {}): 状态 {} - {}",
                        response.processor,
                        response.reply,
                        command::hook_status_name(response.reply)
                    );
                }

                if remove {
                    // 断点没了就不会再有等待项；别把目标进程留在挂起状态。
                    self.thaw_target();
                    self.breakpoint_pid = 0;
                } else if kind == command::HOOK_KIND_USER_BREAKPOINT {
                    self.breakpoint_pid = pid;
                }

                // 一次性断点：命中一次、被继续之后由 hypervisor 禁用。
                if !remove && kind == command::HOOK_KIND_USER_BREAKPOINT && self.hook_one_shot {
                    match self.direct_hypercall(
                        hypercall::FUNCTION_BREAKPOINT_ONESHOT,
                        address,
                        1,
                    ) {
                        Ok(0) => {
                            return format!(
                                "{action}{kind_name}成功（一次性，命中后禁用）: 0x{address:016X} \
                                 (pid {pid}, CPU {})",
                                response.processor
                            );
                        }
                        Ok(status) => {
                            return format!(
                                "{action}{kind_name}成功，但设置一次性失败: 状态 {status}"
                            );
                        }
                        Err(err) => {
                            return format!("{action}{kind_name}成功，但设置一次性失败: {err}")
                        }
                    }
                }

                format!(
                    "{action}{kind_name}成功: 0x{address:016X} (pid {pid}, CPU {})",
                    response.processor
                )
            }
            Err(error) => error,
        }
    }

    /// General ping: VMCALL directly from this process on the CPU that runs
    /// the guest. Multi-core mode uses the current processor (all effective
    /// CPUs are virtualized); single-core mode uses StartProcessor, the only
    /// CPU that ran `_init`.
    fn hypercall_ping(&mut self) -> String {
        // 重新读取而不是相信缓存的快照：Stop 之后缓存可能仍显示 Running。
        let info = match self.request(|device| device.get_info()) {
            Ok(info) => {
                self.info = Some(info);
                info
            }
            Err(err) => return err,
        };

        if !info.is_running() {
            return format!("Hypervisor 未运行（{}）", info.state_name());
        }

        if info.hypercall_key == 0 {
            return String::from("HypercallKey 为 0，hypercall 已禁用");
        }

        let target = if info.single_core != 0 {
            info.start_processor
        } else {
            let current = hypercall::current_processor();

            if current < info.cpu_count {
                current
            } else {
                info.start_processor
            }
        };

        let nonce = self.next_nonce();

        let _pinned = match hypercall::PinnedThread::new(target) {
            Ok(pinned) => pinned,
            Err(err) => {
                return format!(
                    "无法把线程绑定到 CPU {target}: {err}；请改用 Hypercall Ping (单核)"
                );
            }
        };

        // SAFETY: GetInfo reports Running and supplies the key installed by the
        // loader, and `_pinned` puts this thread on a CPU that ran `_init`:
        // StartProcessor in single-core mode, the current CPU in multi-core
        // mode. Without the pin a VMCALL on any other CPU would raise #UD and
        // kill the process.
        let reply = unsafe { hypercall::ping(info.hypercall_key, nonce) };
        let expected = hypercall::expected_pong(nonce);

        if reply == expected {
            format!(
                "Hypercall Ping (直接, CPU {target}) 成功: nonce 0x{nonce:016X}, reply 0x{reply:016X}"
            )
        } else {
            format!(
                "Hypercall 应答不匹配 (CPU {target}): 期望 0x{expected:016X}, 收到 0x{reply:016X}"
            )
        }
    }

    /// Single-core variant: the loader pins to StartProcessor -- the only CPU
    /// that runs the guest in single-core mode -- and serializes the VMCALL
    /// with Start/Stop under its hypervisor lock.
    fn hypercall_ping_single_core(&mut self) -> String {
        let info = match self.request(|device| device.get_info()) {
            Ok(info) => {
                self.info = Some(info);
                info
            }
            Err(err) => return err,
        };

        if !info.is_running() {
            return format!("Hypervisor 未运行（{}）", info.state_name());
        }

        let nonce = self.next_nonce();

        match self.request(|device| device.hypercall_ping(nonce, info.start_processor)) {
            Ok(response) => {
                let expected = hypercall::expected_pong(nonce);

                if response.reply == expected {
                    format!(
                        "Hypercall Ping (单核/驱动, CPU {}) 成功: nonce 0x{nonce:016X}, reply 0x{:016X}",
                        response.processor, response.reply
                    )
                } else {
                    format!(
                        "Hypercall 应答不匹配 (CPU {}): 期望 0x{expected:016X}, 收到 0x{:016X}",
                        response.processor, response.reply
                    )
                }
            }
            Err(err) => err,
        }
    }

}

impl Drop for MyApp {
    fn drop(&mut self) {
        // 退出前 join 后台应答线程：它还持有 hypercall key，Stop 之后继续
        // VMCALL 会 #UD 并带走整个进程。
        if let Some(mut worker) = self.wait_worker.take() {
            worker.stop();
        }

        // 别把被调试进程的线程留在挂起状态。
        if let Some(frozen) = self.frozen.take() {
            let _ = frozen.thaw();
        }
    }
}

impl eframe::App for MyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.auto_poll_waits(ui.ctx());

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Noumen Loader");
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    ui.selectable_value(&mut self.tab, tab, tab.label());
                }
            });
            ui.separator();

            // 每个 tab 的滚动位置独立；切 tab 时复位到顶部，避免页首按钮
            // 落在视口外。
            let tab_changed = self.last_tab != self.tab;
            self.last_tab = self.tab;

            let mut scroll = egui::ScrollArea::vertical()
                .id_salt(("main_scroll", self.tab as u8));

            if tab_changed {
                scroll = scroll.scroll_offset(egui::Vec2::ZERO);
            }

            scroll.show(ui, |ui| {
                tabs::show(self, ui);
                ui.separator();
                ui.label(&self.status);
            });
        });
    }
}
