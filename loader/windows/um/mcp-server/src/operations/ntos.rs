//! ntoskrnl/Windows object traversal used by the high-level REST APIs.
//!
//! All guest reads go through the same MEM_BATCH helpers used by the raw
//! memory operations. The code mirrors `loader/windows/um/uapp/src/os_win.rs`:
//! resolve exports with batched reads, walk `ActiveProcessLinks`, then
//! `PEB -> Ldr -> InLoadOrderModuleList`.

use std::ffi::c_void;
use std::ptr;

use serde_json::{Map, Value, json};
use windows_sys::Win32::System::ProcessStatus::{
    K32EnumDeviceDrivers as EnumDeviceDrivers,
    K32GetDeviceDriverBaseNameW as GetDeviceDriverBaseNameW,
};

use super::{
    Config, GET_EXTRA_DATA, GET_SYSTEM_CR3, MAX_MEM_OPS, OpError, invoke, read_many_virtual,
    read_u16, read_u32, read_u64, read_virtual,
};

#[path = "../ntos_offsets.rs"]
mod offsets;

use offsets::{X64, select};

const MAX_PROCESSES: usize = 4096;
const MAX_MODULES: usize = 1024;
const STATUS_PENDING: u32 = 0x103;
const IMAGE_FILE_NAME_LEN: usize = 15;

struct Ntos {
    ntos_base: u64,
    system_cr3: u64,
    system_eprocess: u64,
    loaded_module_list: u64,
    offsets: &'static offsets::WinOffsets,
}

impl Ntos {
    fn attach(config: &Config, args: &Map<String, Value>) -> Result<Self, OpError> {
        let ntos_base = match args.get("ntos_base") {
            Some(value) => super::parse_u64(value).map_err(OpError::bad_request)?,
            None => match config.ntos_base {
                Some(base) => base,
                None => {
                    let extra = invoke(config, GET_EXTRA_DATA, args, 0, 0, 0)?.rax;
                    if extra != 0 {
                        extra
                    } else {
                        local_ntos_base()?
                    }
                }
            },
        };

        let system_cr3 = invoke(config, GET_SYSTEM_CR3, args, 0, 0, 0)?.rax;
        if system_cr3 == 0 {
            return Err(OpError::unavailable("hypervisor returned system_cr3=0"));
        }

        // Resolve NtBuildNumber, then normalize its high platform bits.
        let build_va = resolve_export(config, args, system_cr3, ntos_base, b"NtBuildNumber")?
            .ok_or_else(|| OpError::unavailable("NtBuildNumber export not found"))?;
        let build = read_u32(&read_virtual(config, args, system_cr3, build_va, 4)?, 0) & 0xFFFF;

        let offsets = select(build).ok_or_else(|| {
            OpError::unavailable(format!("no EPROCESS offsets for build {build}"))
        })?;

        let system_eprocess_va = resolve_export(
            config,
            args,
            system_cr3,
            ntos_base,
            b"PsInitialSystemProcess",
        )?
        .ok_or_else(|| OpError::unavailable("PsInitialSystemProcess export not found"))?;
        let system_eprocess = read_u64(
            &read_virtual(config, args, system_cr3, system_eprocess_va, 8)?,
            0,
        );

        let loaded_module_list =
            resolve_export(config, args, system_cr3, ntos_base, b"PsLoadedModuleList")?
                .ok_or_else(|| OpError::unavailable("PsLoadedModuleList export not found"))?;

        Ok(Self {
            ntos_base,
            system_cr3,
            system_eprocess,
            loaded_module_list,
            offsets,
        })
    }

    fn modules(&self, config: &Config, args: &Map<String, Value>) -> Result<Vec<Value>, OpError> {
        let head = self.loaded_module_list;
        let mut entry = read_u64(&read_virtual(config, args, self.system_cr3, head, 8)?, 0);
        let mut modules = Vec::new();

        for _ in 0..MAX_MODULES {
            if entry == 0 || entry == head {
                break;
            }

            let data = read_virtual(config, args, self.system_cr3, entry, 0x80)?;
            let base = read_u64(&data, 0x30);
            let size = read_u32(&data, 0x40);
            let name =
                read_unicode(config, args, self.system_cr3, entry + 0x58).unwrap_or_default();
            let next = read_u64(&data, 0);

            modules.push(json!({
                "name": name,
                "base": super::hex(base),
                "size": size
            }));

            entry = next;
        }

        Ok(modules)
    }

    fn processes(
        &self,
        config: &Config,
        args: &Map<String, Value>,
    ) -> Result<Vec<ProcessEntry>, OpError> {
        let mut out = Vec::new();
        let mut eprocess = self.system_eprocess;
        let link = self.offsets.eproc_link as u64;

        for _ in 0..MAX_PROCESSES {
            if eprocess == 0 {
                break;
            }

            let data = read_virtual(config, args, self.system_cr3, eprocess, 0x1000)?;
            let pid = read_u32(&data, self.offsets.eproc_pid as usize);
            let dtb = read_u64(&data, self.offsets.kproc_dtb as usize);
            let image_base = read_u64(&data, self.offsets.eproc_section_base as usize);
            let peb = read_u64(&data, self.offsets.eproc_peb as usize);
            let exit_status = read_u32(&data, self.offsets.eproc_exit_status as usize);
            let name_start = self.offsets.eproc_name as usize;
            let name_bytes = &data[name_start..name_start + IMAGE_FILE_NAME_LEN];
            let name_end = name_bytes
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(name_bytes.len());

            out.push(ProcessEntry {
                pid,
                eprocess,
                dtb,
                image_base,
                peb,
                exit_status,
                name: String::from_utf8_lossy(&name_bytes[..name_end]).into_owned(),
            });

            let next = read_u64(&data, link as usize);
            if next == 0 {
                break;
            }

            eprocess = next.wrapping_sub(link);
            if eprocess == self.system_eprocess {
                break;
            }
        }

        Ok(out)
    }

    fn process_modules(
        &self,
        config: &Config,
        args: &Map<String, Value>,
        process: &ProcessEntry,
    ) -> Result<Vec<Value>, OpError> {
        if process.dtb == 0 || process.exit_status != STATUS_PENDING {
            return Err(OpError::unavailable("process is exited or has no DTB"));
        }

        if process.peb == 0 {
            return Err(OpError::unavailable("EPROCESS.Peb is zero"));
        }

        let ldr = read_u64(
            &read_virtual(
                config,
                args,
                process.dtb,
                process.peb + X64.peb_ldr as u64,
                8,
            )?,
            0,
        );

        if ldr == 0 {
            return Err(OpError::unavailable("PEB.Ldr is zero"));
        }

        let head = ldr + X64.ldr_list as u64;
        let mut entry = read_u64(&read_virtual(config, args, process.dtb, head, 8)?, 0);
        let mut modules = Vec::new();

        for _ in 0..MAX_MODULES {
            if entry == 0 || entry == head {
                break;
            }

            let data = read_virtual(config, args, process.dtb, entry, 0x100)?;
            let base = read_u64(&data, X64.ldr_data_base as usize);
            let size = read_u32(&data, X64.ldr_data_size as usize);
            let name = read_unicode(
                config,
                args,
                process.dtb,
                entry + X64.ldr_data_base_name as u64,
            )
            .unwrap_or_default();
            let next = read_u64(&data, 0);

            modules.push(json!({
                "name": name,
                "base": super::hex(base),
                "size": size
            }));

            entry = next;
        }

        Ok(modules)
    }
}

struct ProcessEntry {
    pid: u32,
    eprocess: u64,
    dtb: u64,
    image_base: u64,
    peb: u64,
    exit_status: u32,
    name: String,
}

pub(super) fn kernel_modules(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let ntos = Ntos::attach(config, args)?;
    let modules = ntos.modules(config, args)?;

    Ok(json!({
        "system_cr3": super::hex(ntos.system_cr3),
        "ntos_base": super::hex(ntos.ntos_base),
        "count": modules.len(),
        "modules": modules
    }))
}

pub(super) fn process_list(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let ntos = Ntos::attach(config, args)?;
    let processes = ntos.processes(config, args)?;
    let entries: Vec<_> = processes
        .iter()
        .map(|process| {
            json!({
                "pid": process.pid,
                "name": process.name,
                "eprocess": super::hex(process.eprocess),
                "cr3": super::hex(process.dtb),
                "image_base": super::hex(process.image_base),
                "peb": super::hex(process.peb),
                "exit_status": process.exit_status,
                "alive": process.exit_status == STATUS_PENDING
            })
        })
        .collect();

    Ok(json!({
        "system_cr3": super::hex(ntos.system_cr3),
        "count": entries.len(),
        "processes": entries
    }))
}

pub(super) fn process_modules(
    config: &Config,
    args: &Map<String, Value>,
) -> Result<Value, OpError> {
    let pid = super::required_u64(args, "pid")?;
    let pid = u32::try_from(pid).map_err(|_| OpError::bad_request("pid does not fit in u32"))?;
    let ntos = Ntos::attach(config, args)?;
    let processes = ntos.processes(config, args)?;
    let process = processes
        .iter()
        .find(|process| process.pid == pid)
        .ok_or_else(|| OpError::unavailable(format!("process {pid} not found")))?;
    let modules = ntos.process_modules(config, args, process)?;

    Ok(json!({
        "pid": pid,
        "name": process.name,
        "eprocess": super::hex(process.eprocess),
        "cr3": super::hex(process.dtb),
        "count": modules.len(),
        "modules": modules
    }))
}

fn read_unicode(
    config: &Config,
    args: &Map<String, Value>,
    cr3: u64,
    address: u64,
) -> Result<String, OpError> {
    let header = read_virtual(config, args, cr3, address, 16)?;
    let length = read_u16(&header, 0) as usize;
    let buffer = read_u64(&header, 8);

    if length == 0 || buffer == 0 {
        return Ok(String::new());
    }

    let bytes = read_virtual(config, args, cr3, buffer, length.min(512) as u32)?;
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    Ok(String::from_utf16_lossy(&units))
}

fn resolve_export(
    config: &Config,
    args: &Map<String, Value>,
    cr3: u64,
    base: u64,
    name: &[u8],
) -> Result<Option<u64>, OpError> {
    const NAME_SCAN: u32 = 128;
    const MAX_EXPORT_NAMES: usize = 10000;

    let headers = read_virtual(config, args, cr3, base, 0x1000)?;
    if headers.len() < 0x40 || &headers[..2] != b"MZ" {
        return Err(OpError::unavailable("ntoskrnl has no MZ header"));
    }

    let e_lfanew = read_u32(&headers, 0x3C) as usize;
    if e_lfanew + 0x18 + 0x70 + 8 > headers.len() {
        return Err(OpError::unavailable("ntoskrnl PE header is out of range"));
    }

    if &headers[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
        return Err(OpError::unavailable("ntoskrnl has no PE signature"));
    }

    let optional = e_lfanew + 0x18;
    let export_rva = read_u32(&headers, optional + 0x70) as u64;

    if export_rva == 0 {
        return Err(OpError::unavailable("ntoskrnl has no export directory"));
    }

    let directory = read_virtual(config, args, cr3, base + export_rva, 40)?;
    let number_of_functions = read_u32(&directory, 0x14) as u64;
    let number_of_names = read_u32(&directory, 0x18) as usize;
    let functions = read_u32(&directory, 0x1C) as u64;
    let names = read_u32(&directory, 0x20) as u64;
    let ordinals = read_u32(&directory, 0x24) as u64;

    if number_of_names == 0 || number_of_functions == 0 {
        return Ok(None);
    }

    let limit = number_of_names.min(MAX_EXPORT_NAMES);
    let mut index = 0usize;

    while index < limit {
        let count = MAX_MEM_OPS.min(limit - index);
        let rva_buffer = read_virtual(
            config,
            args,
            cr3,
            base + names + (index * 4) as u64,
            (count * 4) as u32,
        )?;

        let mut reads = Vec::with_capacity(count);
        for chunk in rva_buffer.as_chunks::<4>().0 {
            let rva = u32::from_le_bytes(*chunk) as u64;
            reads.push((base + rva, NAME_SCAN));
        }

        let results = read_many_virtual(config, args, cr3, &reads)?;

        for (slot, result) in results.into_iter().enumerate() {
            let Some(buffer) = result else {
                continue;
            };
            let end = buffer
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(buffer.len());
            if &buffer[..end] != name {
                continue;
            }

            let ordinal_index = index + slot;
            let ordinal = read_u16(
                &read_virtual(
                    config,
                    args,
                    cr3,
                    base + ordinals + (ordinal_index * 2) as u64,
                    2,
                )?,
                0,
            ) as u64;

            if ordinal >= number_of_functions {
                return Err(OpError::unavailable("export ordinal out of range"));
            }

            let rva = read_u32(
                &read_virtual(config, args, cr3, base + functions + ordinal * 4, 4)?,
                0,
            ) as u64;

            return Ok(Some(base + rva));
        }

        index += count;
    }

    Ok(None)
}

fn local_ntos_base() -> Result<u64, OpError> {
    let mut needed = 0u32;
    if unsafe { EnumDeviceDrivers(ptr::null_mut(), 0, &mut needed) } == 0 || needed == 0 {
        return Err(OpError::unavailable("EnumDeviceDrivers size query failed"));
    }

    let count = needed as usize / std::mem::size_of::<*mut c_void>();
    let mut drivers = vec![ptr::null_mut(); count];

    if unsafe { EnumDeviceDrivers(drivers.as_mut_ptr(), needed, &mut needed) } == 0 {
        return Err(OpError::unavailable("EnumDeviceDrivers failed"));
    }

    let mut name = [0u16; 260];

    for base in drivers {
        if base.is_null() {
            continue;
        }

        let length =
            unsafe { GetDeviceDriverBaseNameW(base, name.as_mut_ptr(), name.len() as u32) };
        if length == 0 {
            continue;
        }

        let name = String::from_utf16_lossy(&name[..(length as usize).min(name.len())]);
        if name.to_ascii_lowercase().contains("ntoskrnl.exe") {
            return Ok(base as u64);
        }
    }

    Err(OpError::unavailable("ntoskrnl base not found"))
}
