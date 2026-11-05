//! uapp 的 Windows 层：用 [`MemClient`] 的通用原语自己完成
//!
//! * ntoskrnl 的 PE 解析与 `NtBuildNumber` 读取；
//! * `PsInitialSystemProcess` / `PsLoadedModuleList` 导出解析；
//! * 偏移表选择与自检 V1–V4；
//! * `EPROCESS` 链表、`PEB → Ldr` 模块链表。
//!
//! hypervisor 完全不参与这些事。两条 CR3 规则：
//! 内核对象用 `KernelArgs.system_cr3`，目标进程的用户地址用 `EPROCESS.DirectoryTableBase`。

use uapp::pe;
use uapp::win_offsets::{self, WinOffsets, X64, IMAGE_FILE_NAME_LEN, STATUS_PENDING};

use crate::command;
use crate::mem_client::MemClient;

const MAX_PROCESSES: usize = 4096;
const MAX_MODULES: usize = 512;

/// 进程条目。
#[derive(Debug, Clone, Default)]
pub struct ProcessEntry {
    pub pid: u32,
    pub eprocess: u64,
    pub dtb: u64,
    pub image_base: u64,
    pub peb: u64,
    pub exit_status: u32,
    pub name: String,
}

impl ProcessEntry {
    pub fn alive(&self) -> bool {
        self.exit_status == STATUS_PENDING
    }
}

/// 模块条目。
#[derive(Debug, Clone, Default)]
pub struct ModuleEntry {
    pub base: u64,
    pub size: u32,
    pub time_date_stamp: u32,
    pub name: String,
}

/// 引导结果 + 自检位。
#[derive(Debug, Clone)]
pub struct WinOs {
    pub build: u32,
    pub nt_base: u64,
    pub system_cr3: u64,
    pub offsets: &'static WinOffsets,
    pub system_eprocess: u64,
    pub loaded_module_list: u64,
    /// bit0=V2(build 与驱动一致) bit1=V3(PID 4) bit2=V4(链表) bit3=降级表。
    pub checks: u32,
    pub notes: Vec<String>,
}

impl WinOs {
    /// 解析并校验引导信息。
    ///
    /// 起点（ntoskrnl 基址 / build / System EPROCESS）来自**驱动**的 `GetOsInfo`；
    /// 之后的导出解析、偏移选表、遍历全部在这里完成。
    pub fn attach(
        mem: &mut MemClient,
        system_cr3: u64,
        info: &command::OsInfo,
    ) -> Result<Self, String> {
        if system_cr3 == 0 {
            return Err(String::from("KernelArgs.SystemCr3 为 0"));
        }

        if !info.ntos_ready() {
            return Err(String::from(
                "驱动没有定位到 ntoskrnl（GetOsInfo 返回 NtosBase=0）",
            ));
        }

        let nt_base = info.ntos_base;
        let mut notes = Vec::new();

        // V1：PE 头（少量读，直接走 reader）。
        let headers = {
            let mut reader = |va: u64, out: &mut [u8]| -> Result<(), String> {
                mem.read_virt(system_cr3, va, out).map_err(|error| error.to_string())
            };

            pe::parse_headers(&mut reader, nt_base)?
        };

        // 导出解析走**批量读**：ntoskrnl 约 2500 个导出名，逐个读要几千次
        // VMCALL（实测 attach 会到 40 ms 量级）。`resolve_export` 每批处理
        // 100 个名字，attcach 降到毫秒级。
        let runtime_build = match resolve_export(mem, system_cr3, nt_base, b"NtBuildNumber")? {
            Some(va) => Some(
                mem.read_u32(system_cr3, va)
                    .map_err(|error| error.to_string())?,
            ),
            None => None,
        };

        let exported_system =
            match resolve_export(mem, system_cr3, nt_base, b"PsInitialSystemProcess")? {
                Some(va) => Some(
                    mem.read_u64(system_cr3, va)
                        .map_err(|error| error.to_string())?,
                ),
                None => None,
            };

        // PsLoadedModuleList：导出变量本身就是 LIST_ENTRY 头。
        let loaded_module_va = resolve_export(mem, system_cr3, nt_base, b"PsLoadedModuleList")?;

        if headers.image_size < 0x10_0000 || headers.image_size > 0x4000_0000 {
            return Err(format!("ntoskrnl SizeOfImage 非法: {:#x}", headers.image_size));
        }

        // `NtBuildNumber` 的高位带平台/checked 标志（例如 14393 上是
        // 0xF0003839），必须 `& 0xFFFF` 才是 build 号（与 memflow 的
        // `mask_build_number` 一致）。
        let runtime_build = runtime_build.map(|value| value & 0xFFFF);
        let driver_build = info.build & 0xFFFF;

        let build = runtime_build
            .filter(|build| *build != 0)
            .or((driver_build != 0).then_some(driver_build))
            .ok_or_else(|| String::from("无法确定 NtBuildNumber"))?;

        let offsets = win_offsets::select(build)
            .ok_or_else(|| format!("没有 build {build} 的偏移表（最新到 {}）", win_offsets::newest_build()))?;

        let mut checks = 0u32;

        if driver_build == 0 || driver_build == build {
            checks |= 1;
        } else {
            notes.push(format!(
                "驱动 OsBuild={driver_build} 与运行时 NtBuildNumber={build} 不一致，按运行时值选表"
            ));
        }

        if offsets.build != build {
            checks |= 1 << 3;
            notes.push(format!(
                "没有 build {build} 的精确表，降级使用 {}",
                offsets.build
            ));
        }

        let system_eprocess = match (exported_system, info.system_eprocess) {
            (Some(exported), driver) if driver != 0 && driver != exported => {
                notes.push(format!(
                    "驱动 OsEprocess={driver:#x} 与导出解析 {exported:#x} 不一致，采用导出值"
                ));
                exported
            }
            (Some(exported), _) => exported,
            (None, driver) if driver != 0 => driver,
            (None, _) => return Err(String::from("无法定位 PsInitialSystemProcess")),
        };

        if system_eprocess == 0 {
            return Err(String::from("PsInitialSystemProcess 为空"));
        }

        // V3：System 的 PID 恒为 4，且映像名以 "System" 开头。
        let pid = mem
            .read_u64(system_cr3, system_eprocess + offsets.eproc_pid as u64)
            .map_err(|error| format!("读 System EPROCESS 失败: {error}"))? as u32;

        let mut name = [0u8; IMAGE_FILE_NAME_LEN];
        mem.read_virt(
            system_cr3,
            system_eprocess + offsets.eproc_name as u64,
            &mut name,
        )
        .map_err(|error| format!("读 System 映像名失败: {error}"))?;

        if pid != 4 {
            return Err(format!(
                "自检 V3 失败：System EPROCESS 的 PID 是 {pid}，偏移表可能不匹配"
            ));
        }

        if !name.starts_with(b"System") {
            return Err(String::from("自检 V3 失败：System 映像名不符"));
        }

        checks |= 1 << 1;

        let loaded_module_list = match loaded_module_va {
            Some(va) => va,
            None => {
                notes.push(String::from("找不到 PsLoadedModuleList，内核模块枚举不可用"));
                0
            }
        };

        let mut os = Self {
            build,
            nt_base,
            system_cr3,
            offsets,
            system_eprocess,
            loaded_module_list,
            checks,
            notes,
        };

        // V4：进程链表能走一圈。
        let (count, _) = os.walk_processes(mem, MAX_PROCESSES)?;

        if count <= 4 {
            return Err(format!("自检 V4 失败：只遍历到 {count} 个进程"));
        }

        os.checks |= 1 << 2;

        Ok(os)
    }

    fn walk_processes(
        &self,
        mem: &mut MemClient,
        limit: usize,
    ) -> Result<(usize, Vec<ProcessEntry>), String> {
        let mut out = Vec::new();
        let mut eprocess = self.system_eprocess;
        let link = self.offsets.eproc_link as u64;

        for _ in 0..limit {
            let pid = mem
                .read_u64(self.system_cr3, eprocess + self.offsets.eproc_pid as u64)
                .map_err(|error| error.to_string())? as u32;
            let dtb = mem
                .read_u64(self.system_cr3, eprocess + self.offsets.kproc_dtb as u64)
                .map_err(|error| error.to_string())?;
            let image_base = mem
                .read_u64(self.system_cr3, eprocess + self.offsets.eproc_section_base as u64)
                .unwrap_or(0);
            let peb = mem
                .read_u64(self.system_cr3, eprocess + self.offsets.eproc_peb as u64)
                .unwrap_or(0);
            let exit_status = mem
                .read_u32(self.system_cr3, eprocess + self.offsets.eproc_exit_status as u64)
                .unwrap_or(STATUS_PENDING);

            let mut name = [0u8; IMAGE_FILE_NAME_LEN];
            mem.read_virt(self.system_cr3, eprocess + self.offsets.eproc_name as u64, &mut name)
                .unwrap_or(());

            let name_len = name.iter().position(|byte| *byte == 0).unwrap_or(name.len());

            out.push(ProcessEntry {
                pid,
                eprocess,
                dtb,
                image_base,
                peb,
                exit_status,
                name: String::from_utf8_lossy(&name[..name_len]).into_owned(),
            });

            let next = mem
                .read_u64(self.system_cr3, eprocess + link)
                .map_err(|error| error.to_string())?;

            if next == 0 {
                break;
            }

            eprocess = next.wrapping_sub(link);

            if eprocess == self.system_eprocess {
                break;
            }
        }

        Ok((out.len(), out))
    }

    /// 遍历进程链表。
    pub fn processes(&self, mem: &mut MemClient) -> Result<Vec<ProcessEntry>, String> {
        let (_, processes) = self.walk_processes(mem, MAX_PROCESSES)?;
        Ok(processes)
    }

    pub fn process_by_pid(&self, mem: &mut MemClient, pid: u32) -> Result<Option<ProcessEntry>, String> {
        Ok(self.processes(mem)?.into_iter().find(|entry| entry.pid == pid))
    }


    /// 进程的用户 CR3（`DirectoryTableBase`）。
    pub fn process_cr3(&self, process: &ProcessEntry) -> Option<u64> {
        (process.dtb != 0 && process.alive()).then_some(process.dtb)
    }

    /// PEB → Ldr → InLoadOrderModuleList。
    pub fn modules(&self, mem: &mut MemClient, process: &ProcessEntry) -> Result<Vec<ModuleEntry>, String> {
        let Some(cr3) = self.process_cr3(process) else {
            return Err(String::from("进程没有可用的 DTB（可能已退出）"));
        };

        if process.peb == 0 {
            return Err(String::from("EPROCESS.Peb 为 0（可能已退出或偏移不对）"));
        }

        let ldr = mem
            .read_u64(cr3, process.peb + X64.peb_ldr as u64)
            .map_err(|error| format!("读 PEB.Ldr 失败: {error}"))?;

        if ldr == 0 {
            return Err(String::from("PEB.Ldr 为 0"));
        }

        let head = ldr + X64.ldr_list as u64;
        let mut entry = mem
            .read_u64(cr3, head)
            .map_err(|error| format!("读模块链表头失败: {error}"))?;

        let mut out = Vec::new();

        for _ in 0..MAX_MODULES {
            if entry == 0 || entry == head {
                break;
            }

            let base = mem
                .read_u64(cr3, entry + X64.ldr_data_base as u64)
                .unwrap_or(0);
            let size = mem
                .read_u32(cr3, entry + X64.ldr_data_size as u64)
                .unwrap_or(0);

            let name = read_unicode(mem, cr3, entry + X64.ldr_data_base_name as u64).unwrap_or_default();

            // 时间戳失败不影响列表本身。
            let mut time_date_stamp = 0u32;

            if base != 0 {
                let mut reader = |va: u64, out: &mut [u8]| -> Result<(), String> {
                    mem.read_virt(cr3, va, out).map_err(|error| error.to_string())
                };

                if let Ok(headers) = pe::parse_headers(&mut reader, base) {
                    time_date_stamp = headers.time_date_stamp;
                }
            }

            out.push(ModuleEntry {
                base,
                size,
                time_date_stamp,
                name,
            });

            entry = match mem.read_u64(cr3, entry) {
                Ok(next) => next,
                Err(_) => break,
            };
        }

        Ok(out)
    }

    /// 内核模块（`PsLoadedModuleList`）。
    pub fn kernel_modules(&self, mem: &mut MemClient) -> Result<Vec<ModuleEntry>, String> {
        if self.loaded_module_list == 0 {
            return Err(String::from("没有 PsLoadedModuleList"));
        }

        let head = self.loaded_module_list;
        let mut entry = mem
            .read_u64(self.system_cr3, head)
            .map_err(|error| error.to_string())?;

        let mut out = Vec::new();

        for _ in 0..MAX_MODULES {
            if entry == 0 || entry == head {
                break;
            }

            // InLoadOrderLinks 是 KLDR_DATA_TABLE_ENTRY 的第一个字段，可直接当结构基址。
            let base = mem
                .read_u64(self.system_cr3, entry + X64.ldr_data_base as u64)
                .unwrap_or(0);
            let size = mem
                .read_u32(self.system_cr3, entry + X64.ldr_data_size as u64)
                .unwrap_or(0);
            let name = read_unicode(mem, self.system_cr3, entry + X64.ldr_data_base_name as u64)
                .unwrap_or_default();

            out.push(ModuleEntry {
                base,
                size,
                time_date_stamp: 0,
                name,
            });

            entry = match mem.read_u64(self.system_cr3, entry) {
                Ok(next) => next,
                Err(_) => break,
            };
        }

        Ok(out)
    }


    /// 自检位的中文说明（UI 显示）。
    pub fn check_summary(&self) -> String {
        let mut parts = Vec::new();

        if self.checks & 1 != 0 {
            parts.push("build 一致");
        }
        if self.checks & 2 != 0 {
            parts.push("PID 4 自检");
        }
        if self.checks & 4 != 0 {
            parts.push("链表可走");
        }
        if self.checks & 8 != 0 {
            parts.push("降级偏移表");
        }

        parts.join(", ")
    }
}

/// 用**批量读**解析导出：返回导出符号的 VA。
///
/// 逐名调用 `pe::export_rva` 每个名字要一次 VMCALL（ntoskrnl 约 2500 个名字，
/// attach 会到 40 ms 量级）。这里每批处理 `CHUNK` 个名字：
/// 名字 RVA 数组一次 400 B 读，名字字符串一次 `read_many`（≤126 个），
/// 整张表 ~3 次批量往返。
fn resolve_export(
    mem: &mut MemClient,
    cr3: u64,
    base: u64,
    name: &[u8],
) -> Result<Option<u64>, String> {
    const CHUNK: usize = 100;
    const NAME_SCAN: usize = 128;

    let headers = {
        let mut reader = |va: u64, out: &mut [u8]| -> Result<(), String> {
            mem.read_virt(cr3, va, out).map_err(|error| error.to_string())
        };

        pe::parse_headers(&mut reader, base)?
    };

    if headers.export_rva == 0 {
        return Ok(None);
    }

    let mut directory = [0u8; 40];

    mem.read_virt(cr3, base + headers.export_rva as u64, &mut directory)
        .map_err(|error| format!("读导出目录失败: {error}"))?;

    let number_of_functions =
        u32::from_le_bytes(directory[0x14..0x18].try_into().unwrap()) as usize;
    let number_of_names = u32::from_le_bytes(directory[0x18..0x1C].try_into().unwrap()) as usize;
    let functions = u32::from_le_bytes(directory[0x1C..0x20].try_into().unwrap()) as u64;
    let names = u32::from_le_bytes(directory[0x20..0x24].try_into().unwrap()) as u64;
    let ordinals = u32::from_le_bytes(directory[0x24..0x28].try_into().unwrap()) as u64;

    if number_of_names == 0 || number_of_functions == 0 {
        return Ok(None);
    }

    let limit = number_of_names.min(pe::MAX_EXPORT_NAMES);
    let mut index = 0usize;

    while index < limit {
        let count = CHUNK.min(limit - index);
        let mut rva_buffer = vec![0u8; count * 4];

        mem.read_virt(cr3, base + names + index as u64 * 4, &mut rva_buffer)
            .map_err(|error| format!("读导出名数组失败: {error}"))?;

        let mut requests = Vec::with_capacity(count);

        for chunk in rva_buffer.chunks_exact(4) {
            let rva = u32::from_le_bytes(chunk.try_into().unwrap()) as u64;
            requests.push((base + rva, NAME_SCAN));
        }

        let results = mem
            .read_many(cr3, &requests)
            .map_err(|error| format!("批量读导出名失败: {error}"))?;

        for (slot, result) in results.into_iter().enumerate() {
            let Some(buffer) = result else {
                continue;
            };

            let end = buffer.iter().position(|byte| *byte == 0).unwrap_or(buffer.len());

            if &buffer[..end] != name {
                continue;
            }

            let ordinal_index = index + slot;

            let ordinal = mem
                .read_u16(cr3, base + ordinals + ordinal_index as u64 * 2)
                .map_err(|error| format!("读导出序号失败: {error}"))? as usize;

            if ordinal >= number_of_functions {
                return Err(format!("导出序号越界: {ordinal}"));
            }

            let rva = mem
                .read_u32(cr3, base + functions + ordinal as u64 * 4)
                .map_err(|error| format!("读导出函数 RVA 失败: {error}"))?;

            return Ok(Some(base + rva as u64));
        }

        index += count;
    }

    Ok(None)
}

/// 读 `UNICODE_STRING`（Length/MaximumLength/Buffer），上限 512 字节。
fn read_unicode(mem: &mut MemClient, cr3: u64, address: u64) -> Result<String, String> {
    let mut header = [0u8; 16];
    mem.read_virt(cr3, address, &mut header)
        .map_err(|error| error.to_string())?;

    let length = u16::from_le_bytes([header[0], header[1]]) as usize;
    let buffer = u64::from_le_bytes(header[8..16].try_into().unwrap());

    if buffer == 0 || length == 0 {
        return Ok(String::new());
    }

    let length = length.min(512);
    let mut bytes = vec![0u8; length];
    mem.read_virt(cr3, buffer, &mut bytes)
        .map_err(|error| error.to_string())?;

    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    Ok(String::from_utf16_lossy(&units))
}
