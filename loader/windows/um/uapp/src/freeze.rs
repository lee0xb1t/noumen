//! 让“被调试进程”整体停住。
//!
//! 我们的 EPT 用户断点只在**命中的那个线程**上自旋 `INT3`，进程里其它线程
//! 照常运行（所以控制台还会继续输出）。真实调试器的语义是“命中即整个进程
//! 暂停”，因此 uapp 在等待项出现后把目标进程的所有线程 `SuspendThread`，
//! 应答/放行后再恢复。
//!
//! 命中的那个线程也一起挂起：它的 RIP 停在 `INT3` 上，恢复后重新执行 `0xCC`
//! 会再次触发 `#BP`，等待项的应答就在那一刻被应用。

use std::mem::size_of;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
};

/// 已挂起的目标线程；`thaw` 时逐个恢复并关闭句柄。
pub struct Frozen {
    pid: u32,
    threads: Vec<HANDLE>,
}

impl Frozen {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn count(&self) -> usize {
        self.threads.len()
    }

    /// 恢复所有线程；返回恢复成功的个数。幂等：句柄只关一次。
    pub fn thaw(mut self) -> usize {
        let mut resumed = 0;

        for handle in self.threads.drain(..) {
            if unsafe { ResumeThread(handle) } != u32::MAX {
                resumed += 1;
            }

            unsafe {
                CloseHandle(handle);
            }
        }

        resumed
    }
}

/// 挂起 `pid` 的所有线程（包括正在断点上自旋的那个）。
pub fn freeze(pid: u32) -> Result<Frozen, String> {
    if pid == 0 {
        return Err(String::from("pid 0 不能挂起"));
    }

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };

    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return Err(String::from("CreateToolhelp32Snapshot 失败"));
    }

    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;

    let mut threads = Vec::new();
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;

    while more {
        if entry.th32OwnerProcessID == pid {
            let handle = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };

            if !handle.is_null() {
                if unsafe { SuspendThread(handle) } != u32::MAX {
                    threads.push(handle);
                } else {
                    unsafe {
                        CloseHandle(handle);
                    }
                }
            }
        }

        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }

    unsafe {
        CloseHandle(snapshot);
    }

    if threads.is_empty() {
        return Err(format!("pid {pid} 没有可挂起的线程（进程已退出？）"));
    }

    Ok(Frozen { pid, threads })
}

