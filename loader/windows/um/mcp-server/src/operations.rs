//! Named hypercall operations shared by the REST API and MCP tools.
//!
//! The public surface never exposes numeric hypercall commands. The numbers
//! below are private ABI details confined to this module.

use std::collections::{BTreeMap, HashSet};
use std::ffi::c_void;
use std::ptr;

use serde_json::{Map, Value, json};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualLock,
    VirtualUnlock,
};

use crate::{Config, hypercall};

mod ntos;

const PING: u64 = 3;
const HIDE_SELF: u64 = 9;
const READ_SELF: u64 = 10;
const READ_SELF_EPT: u64 = 11;
const HOOK_APPLY: u64 = 20;
const HOOK_RETIRED: u64 = 22;

const FLUSH_LOGS: u64 = 1;
const LOGGER_INFO: u64 = 2;
const MEM_QUERY: u64 = 0x20;
const MEM_BATCH: u64 = 0x21;
const MEM_MAP: u64 = 0x23;
const GET_SYSTEM_CR3: u64 = 0x27;
const GET_EXTRA_DATA: u64 = 0x28;
const SET_EXTRA_DATA: u64 = 0x29;

const MEM_OP_READ_VIRT: u16 = 0;
const MEM_OP_WRITE_VIRT: u16 = 1;
const MEM_OP_TRANSLATE: u16 = 4;

const PONG_MAGIC: u64 = 0x504F_4E47;
const MEM_RING_MAGIC: u32 = 0x4D45_4D4E;
const MEM_DATA_OFFSET: usize = 0x1000;
const MEM_ENTRY_OFFSET: usize = 0x40;
const MEM_ENTRY_CAPACITY: u16 = 126;
const MAX_MEM_OP: usize = 0x1000;
const MEM_RING_BYTES: usize = 64 * 1024;
const MAX_MEM_OPS: usize = 1;
const MAX_LOG_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }

    pub fn matches(self, method: &str) -> bool {
        method.eq_ignore_ascii_case(self.as_str())
    }
}

pub struct Operation {
    pub name: &'static str,
    pub method: Method,
    pub path: &'static str,
    pub summary: &'static str,
    pub handler: fn(&Config, &Map<String, Value>) -> Result<Value, OpError>,
    pub schema: fn() -> Value,
}

#[derive(Debug)]
pub struct OpError {
    pub status: u16,
    pub message: String,
}

impl OpError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: 503,
            message: message.into(),
        }
    }

    pub fn is_bad_request(&self) -> bool {
        self.status == 400
    }
}

#[derive(Debug)]
struct LockedBuffer {
    ptr: *mut u8,
    len: usize,
}

impl LockedBuffer {
    fn new(len: usize) -> Result<Self, OpError> {
        let ptr =
            unsafe { VirtualAlloc(ptr::null(), len, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) }
                as *mut u8;

        if ptr.is_null() {
            return Err(OpError::unavailable(
                "VirtualAlloc for MEM_BATCH ring failed",
            ));
        }

        // uapp treats the lock as best-effort. A failed lock only means the
        // ring page may be trimmed later; the hypercall can still read it.
        // Do not turn this into a fatal API error.
        let _ = unsafe { VirtualLock(ptr.cast::<c_void>(), len) };

        Ok(Self { ptr, len })
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for LockedBuffer {
    fn drop(&mut self) {
        unsafe {
            let _ = VirtualUnlock(self.ptr.cast::<c_void>(), self.len);
            let _ = VirtualFree(self.ptr.cast::<c_void>(), 0, MEM_RELEASE);
        }
    }
}

pub fn all() -> &'static [Operation] {
    const OPERATIONS: &[Operation] = &[
        Operation {
            name: "ping",
            method: Method::Get,
            path: "/api/ping",
            summary: "Ping the hypervisor and verify the PONG reply.",
            handler: ping,
            schema: ping_schema,
        },
        Operation {
            name: "logger_info",
            method: Method::Get,
            path: "/api/logs",
            summary: "Read log-ring totals: total, dropped, buffered.",
            handler: logger_info,
            schema: empty_schema,
        },
        Operation {
            name: "flush_logs",
            method: Method::Post,
            path: "/api/logs/flush",
            summary: "Copy buffered log records into a caller-sized buffer.",
            handler: flush_logs,
            schema: flush_logs_schema,
        },
        Operation {
            name: "hide_self",
            method: Method::Post,
            path: "/api/self/hide",
            summary: "Hide the hypervisor image pages from the guest.",
            handler: hide_self,
            schema: empty_schema,
        },
        Operation {
            name: "read_self",
            method: Method::Get,
            path: "/api/self/read",
            summary: "Read one hypervisor image page through root or EPT mapping.",
            handler: read_self,
            schema: read_self_schema,
        },
        Operation {
            name: "mem_query",
            method: Method::Get,
            path: "/api/memory",
            summary: "Query the memory-service ABI and limits.",
            handler: mem_query,
            schema: empty_schema,
        },
        Operation {
            name: "mem_map",
            method: Method::Get,
            path: "/api/memory/map",
            summary: "Return the physical memory ranges supplied by the loader.",
            handler: mem_map,
            schema: mem_map_schema,
        },
        Operation {
            name: "mem_cr3",
            method: Method::Get,
            path: "/api/memory/cr3",
            summary: "Read the hypervisor's current system CR3.",
            handler: mem_cr3,
            schema: empty_schema,
        },
        Operation {
            name: "mem_read",
            method: Method::Get,
            path: "/api/memory/read",
            summary: "Read guest virtual memory through MEM_BATCH.",
            handler: mem_read,
            schema: mem_read_schema,
        },
        Operation {
            name: "mem_read_many",
            method: Method::Post,
            path: "/api/memory/read-many",
            summary: "Read one guest virtual-memory range per MEM_BATCH (64 KiB ring).",
            handler: mem_read_many,
            schema: mem_read_many_schema,
        },
        Operation {
            name: "mem_write",
            method: Method::Post,
            path: "/api/memory/write",
            summary: "Write guest virtual memory through MEM_BATCH.",
            handler: mem_write,
            schema: mem_write_schema,
        },
        Operation {
            name: "mem_translate",
            method: Method::Get,
            path: "/api/memory/translate",
            summary: "Translate a guest virtual address through MEM_BATCH.",
            handler: mem_translate,
            schema: mem_translate_schema,
        },
        Operation {
            name: "ntos_kernel_modules",
            method: Method::Get,
            path: "/api/ntos/kernel-modules",
            summary: "Enumerate kernel modules through PsLoadedModuleList.",
            handler: ntos::kernel_modules,
            schema: ntos_schema,
        },
        Operation {
            name: "ntos_processes",
            method: Method::Get,
            path: "/api/ntos/processes",
            summary: "Enumerate processes through ActiveProcessLinks.",
            handler: ntos::process_list,
            schema: ntos_schema,
        },
        Operation {
            name: "ntos_process_modules",
            method: Method::Get,
            path: "/api/ntos/processes/modules",
            summary: "Enumerate a process's modules through PEB/Ldr.",
            handler: ntos::process_modules,
            schema: ntos_process_modules_schema,
        },
        Operation {
            name: "extra_data_get",
            method: Method::Get,
            path: "/api/extra-data",
            summary: "Read the hypervisor's opaque 8-byte extra data.",
            handler: extra_data_get,
            schema: empty_schema,
        },
        Operation {
            name: "extra_data_set",
            method: Method::Post,
            path: "/api/extra-data/set",
            summary: "Set the hypervisor's opaque 8-byte extra data.",
            handler: extra_data_set,
            schema: extra_data_set_schema,
        },
        Operation {
            name: "hooks_apply",
            method: Method::Post,
            path: "/api/hooks/apply",
            summary: "Ask the current CPU to apply the latest hook generation.",
            handler: hooks_apply,
            schema: empty_schema,
        },
        Operation {
            name: "hooks_retired",
            method: Method::Get,
            path: "/api/hooks/retired",
            summary: "Check whether all online vCPUs retired the latest hook generation.",
            handler: hooks_retired,
            schema: empty_schema,
        },
    ];

    OPERATIONS
}

pub fn find(name: &str) -> Option<&'static Operation> {
    all().iter().find(|operation| operation.name == name)
}

pub fn find_path(path: &str) -> Option<&'static Operation> {
    all().iter().find(|operation| operation.path == path)
}

pub fn parameter_names(operation: &Operation) -> Vec<(String, bool)> {
    let schema = (operation.schema)();
    let required: HashSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .keys()
                .map(|name| (name.clone(), required.contains(name.as_str())))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parameters_text(operation: &Operation) -> String {
    let parameters: Vec<_> = parameter_names(operation)
        .into_iter()
        .map(
            |(name, required)| {
                if required { format!("{name}*") } else { name }
            },
        )
        .collect();

    if parameters.is_empty() {
        "none".to_owned()
    } else {
        parameters.join(", ")
    }
}

fn ping(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let nonce = get_u64(args, "nonce", 0)?;
    let result = invoke(config, PING, args, nonce, 0, 0)?;
    let expected = PONG_MAGIC ^ nonce;

    Ok(json!({
        "nonce": hex(nonce),
        "reply": hex(result.rax),
        "expected": hex(expected),
        "ok": result.rax == expected
    }))
}

fn logger_info(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let result = invoke(config, LOGGER_INFO, args, 0, 0, 0)?;

    Ok(json!({
        "total": result.rax,
        "dropped": result.rdx,
        "buffered": result.r8
    }))
}

fn flush_logs(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let capacity = get_u64(args, "capacity", 64 * 1024)?.clamp(1, MAX_LOG_BYTES as u64) as usize;
    let mut buffer = vec![0u8; capacity];
    let result = invoke(
        config,
        FLUSH_LOGS,
        args,
        buffer.as_mut_ptr() as u64,
        capacity as u64,
        0,
    )?;
    let records = (result.rax as usize).min(capacity / 152);
    let bytes = records * 152;

    Ok(json!({
        "records": records,
        "bytes": bytes,
        "data": encode_hex(&buffer[..bytes])
    }))
}

fn hide_self(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let result = invoke(config, HIDE_SELF, args, 0, 0, 0)?;

    Ok(json!({ "hidden_pages": result.rax }))
}

fn read_self(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let page = get_u64(args, "page", 0)?;
    let view = get_string(args, "view").unwrap_or_else(|| "root".to_owned());
    let length = get_u64(args, "length", 4096)?.clamp(1, 4096) as usize;
    let command = match view.as_str() {
        "root" => READ_SELF,
        "ept" => READ_SELF_EPT,
        _ => return Err(OpError::bad_request("view must be root or ept")),
    };

    let mut buffer = vec![0u8; 4096];
    let result = invoke(config, command, args, page, buffer.as_mut_ptr() as u64, 0)?;
    let bytes = (result.rax as usize).min(length);

    Ok(json!({
        "page": page,
        "view": view,
        "bytes": bytes,
        "data": encode_hex(&buffer[..bytes])
    }))
}

fn mem_query(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let result = invoke(config, MEM_QUERY, args, 0, 0, 0)?;

    Ok(json!({
        "abi": result.rax,
        "max_ops": result.rdx,
        "max_batch_bytes": result.r8,
        "direct_map": hex(result.r9)
    }))
}

fn mem_map(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let capacity = get_u64(args, "capacity", 64)?.clamp(1, 1024) as usize;
    let mut buffer = vec![0u8; capacity * 16];
    let result = invoke(
        config,
        MEM_MAP,
        args,
        buffer.as_mut_ptr() as u64,
        capacity as u64,
        0,
    )?;
    let count = (result.rax as usize).min(capacity);
    let mut ranges = Vec::with_capacity(count);

    for index in 0..count {
        let offset = index * 16;
        let start = read_u64(&buffer, offset);
        let size = read_u64(&buffer, offset + 8);
        ranges.push(json!({
            "start": hex(start),
            "size": hex(size)
        }));
    }

    Ok(json!({ "count": result.rax, "ranges": ranges }))
}

fn mem_cr3(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let result = invoke(config, GET_SYSTEM_CR3, args, 0, 0, 0)?;

    Ok(json!({ "system_cr3": hex(result.rax) }))
}

fn mem_read(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let cr3 = required_u64(args, "cr3")?;
    let address = required_u64(args, "address")?;
    let length = get_u64(args, "length", 4096)?.clamp(1, MAX_MEM_OP as u64) as u32;
    let (ring, _) = run_mem_batch(
        config,
        args,
        &[MemRequest {
            operation: MEM_OP_READ_VIRT,
            cr3,
            address,
            length,
            payload: None,
        }],
    )?;
    let status = ring_status(ring.as_slice());
    let bytes = if status == 0 { length as usize } else { 0 };

    Ok(json!({
        "status": status,
        "status_name": mem_error_name(status),
        "address": hex(address),
        "data": encode_hex(&ring.as_slice()[MEM_DATA_OFFSET..MEM_DATA_OFFSET + bytes])
    }))
}

fn mem_read_many(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let requests = args
        .get("requests")
        .and_then(Value::as_array)
        .ok_or_else(|| OpError::bad_request("requests must be an array"))?;

    if requests.is_empty() || requests.len() > MAX_MEM_OPS {
        return Err(OpError::bad_request(format!(
            "requests must contain 1..={MAX_MEM_OPS} entries"
        )));
    }

    let mut batch = Vec::with_capacity(requests.len());
    let mut requested = Vec::with_capacity(requests.len());

    for request in requests {
        let request = request
            .as_object()
            .ok_or_else(|| OpError::bad_request("each request must be an object"))?;
        let cr3 = required_u64(request, "cr3")?;
        let address = required_u64(request, "address")?;
        let length = get_u64(request, "length", 4096)?.clamp(1, MAX_MEM_OP as u64) as u32;

        requested.push((address, length));
        batch.push(MemRequest {
            operation: MEM_OP_READ_VIRT,
            cr3,
            address,
            length,
            payload: None,
        });
    }

    let (ring, _) = run_mem_batch(config, args, &batch)?;
    let mut entries = Vec::with_capacity(batch.len());

    for (index, (address, length)) in requested.iter().enumerate() {
        let entry = MEM_ENTRY_OFFSET + index * 32;
        let data = MEM_DATA_OFFSET + index * MAX_MEM_OP;
        let status = read_u16(ring.as_slice(), entry + 2);
        let written = read_u32(ring.as_slice(), entry + 4);
        let bytes = if status == 0 {
            (written as usize).min(*length as usize)
        } else {
            0
        };

        entries.push(json!({
            "status": status,
            "status_name": mem_error_name(status),
            "address": hex(*address),
            "length": length,
            "data": encode_hex(&ring.as_slice()[data..data + bytes])
        }));
    }

    Ok(json!({ "count": entries.len(), "entries": entries }))
}

fn mem_write(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let cr3 = required_u64(args, "cr3")?;
    let address = required_u64(args, "address")?;
    let data = get_hex_data(args, "data")?;

    if data.is_empty() || data.len() > MAX_MEM_OP {
        return Err(OpError::bad_request(format!(
            "data must contain 1..={MAX_MEM_OP} bytes"
        )));
    }

    let length = data.len() as u32;
    let (ring, _) = run_mem_batch(
        config,
        args,
        &[MemRequest {
            operation: MEM_OP_WRITE_VIRT,
            cr3,
            address,
            length,
            payload: Some(&data),
        }],
    )?;
    let status = ring_status(ring.as_slice());

    Ok(json!({
        "status": status,
        "status_name": mem_error_name(status),
        "address": hex(address),
        "bytes": if status == 0 { length } else { 0 }
    }))
}

fn mem_translate(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let cr3 = required_u64(args, "cr3")?;
    let address = required_u64(args, "address")?;
    let (ring, _) = run_mem_batch(
        config,
        args,
        &[MemRequest {
            operation: MEM_OP_TRANSLATE,
            cr3,
            address,
            length: 0,
            payload: None,
        }],
    )?;
    let status = ring_status(ring.as_slice());
    let physical = if status == 0 {
        read_u64(ring.as_slice(), MEM_DATA_OFFSET)
    } else {
        0
    };

    Ok(json!({
        "status": status,
        "status_name": mem_error_name(status),
        "address": hex(address),
        "physical": hex(physical)
    }))
}

fn read_virtual(
    config: &Config,
    args: &Map<String, Value>,
    cr3: u64,
    address: u64,
    length: u32,
) -> Result<Vec<u8>, OpError> {
    if length == 0 {
        return Ok(Vec::new());
    }

    let (ring, _) = run_mem_batch(
        config,
        args,
        &[MemRequest {
            operation: MEM_OP_READ_VIRT,
            cr3,
            address,
            length,
            payload: None,
        }],
    )?;
    let status = ring_status(ring.as_slice());

    if status != 0 {
        return Err(OpError::unavailable(format!(
            "read {address:#x} failed: status={status} ({})",
            mem_error_name(status)
        )));
    }

    Ok(ring.as_slice()[MEM_DATA_OFFSET..MEM_DATA_OFFSET + length as usize].to_vec())
}

fn read_many_virtual(
    config: &Config,
    args: &Map<String, Value>,
    cr3: u64,
    reads: &[(u64, u32)],
) -> Result<Vec<Option<Vec<u8>>>, OpError> {
    if reads.is_empty() {
        return Ok(Vec::new());
    }

    let mut requests = Vec::with_capacity(reads.len());
    for (address, length) in reads {
        requests.push(MemRequest {
            operation: MEM_OP_READ_VIRT,
            cr3,
            address: *address,
            length: *length,
            payload: None,
        });
    }

    let (ring, _) = run_mem_batch(config, args, &requests)?;
    let mut results = Vec::with_capacity(reads.len());

    for (index, (_, length)) in reads.iter().enumerate() {
        let entry = MEM_ENTRY_OFFSET + index * 32;
        let data = MEM_DATA_OFFSET + index * MAX_MEM_OP;
        let status = read_u16(ring.as_slice(), entry + 2);
        let written = read_u32(ring.as_slice(), entry + 4);

        if status == 0 {
            let bytes = (written as usize).min(*length as usize);
            results.push(Some(ring.as_slice()[data..data + bytes].to_vec()));
        } else {
            results.push(None);
        }
    }

    Ok(results)
}

fn extra_data_get(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let result = invoke(config, GET_EXTRA_DATA, args, 0, 0, 0)?;

    Ok(json!({ "value": hex(result.rax) }))
}

fn extra_data_set(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let value = required_u64(args, "value")?;
    invoke(config, SET_EXTRA_DATA, args, value, 0, 0)?;

    Ok(json!({ "value": hex(value) }))
}

fn hooks_apply(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let result = invoke(config, HOOK_APPLY, args, 0, 0, 0)?;

    Ok(json!({ "status": result.rax }))
}

fn hooks_retired(config: &Config, args: &Map<String, Value>) -> Result<Value, OpError> {
    let result = invoke(config, HOOK_RETIRED, args, 0, 0, 0)?;

    Ok(json!({
        "retired": result.rax != 0,
        "raw": result.rax
    }))
}

fn invoke(
    config: &Config,
    command: u64,
    args: &Map<String, Value>,
    arg0: u64,
    arg1: u64,
    arg2: u64,
) -> Result<hypercall::VmcallResult, OpError> {
    let processor = get_processor(args, config.processor)?;

    hypercall::call(config.key, command, arg0, arg1, arg2, processor).map_err(OpError::unavailable)
}

fn get_processor(args: &Map<String, Value>, fallback: Option<u32>) -> Result<Option<u32>, OpError> {
    let Some(value) = args.get("processor") else {
        return Ok(fallback);
    };

    if let Value::String(text) = value
        && text.eq_ignore_ascii_case("any")
    {
        return Ok(None);
    }

    let processor = parse_u64(value).map_err(OpError::bad_request)?;
    let processor = u32::try_from(processor)
        .map_err(|_| OpError::bad_request("processor does not fit in u32"))?;

    Ok(Some(processor))
}

fn get_u64(args: &Map<String, Value>, key: &str, default: u64) -> Result<u64, OpError> {
    match args.get(key) {
        Some(value) => parse_u64(value).map_err(OpError::bad_request),
        None => Ok(default),
    }
}

fn required_u64(args: &Map<String, Value>, key: &str) -> Result<u64, OpError> {
    let value = args
        .get(key)
        .ok_or_else(|| OpError::bad_request(format!("{key} is required")))?;

    parse_u64(value).map_err(OpError::bad_request)
}

fn get_string(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

fn parse_u64(value: &Value) -> Result<u64, String> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| "expected a non-negative integer".to_owned()),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Err("empty integer".to_owned());
            }

            if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16).map_err(|error| error.to_string())
            } else {
                text.parse::<u64>().map_err(|error| error.to_string())
            }
        }
        _ => Err("expected an integer or a decimal/hex string".to_owned()),
    }
}

fn get_hex_data(args: &Map<String, Value>, key: &str) -> Result<Vec<u8>, OpError> {
    let text = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| OpError::bad_request(format!("{key} must be a hex string")))?;
    let text = text.trim();
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    let text: String = text
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();

    if !text.len().is_multiple_of(2) {
        return Err(OpError::bad_request("hex data must have an even length"));
    }

    let mut bytes = Vec::with_capacity(text.len() / 2);
    for index in (0..text.len()).step_by(2) {
        let byte = u8::from_str_radix(&text[index..index + 2], 16)
            .map_err(|error| OpError::bad_request(format!("invalid hex data: {error}")))?;
        bytes.push(byte);
    }

    Ok(bytes)
}

struct MemRequest<'a> {
    operation: u16,
    cr3: u64,
    address: u64,
    length: u32,
    payload: Option<&'a [u8]>,
}

fn build_mem_ring(requests: &[MemRequest<'_>]) -> Result<(LockedBuffer, usize), OpError> {
    if requests.is_empty() || requests.len() > MAX_MEM_OPS {
        return Err(OpError::bad_request(format!(
            "MEM_BATCH requires 1..={MAX_MEM_OPS} entries"
        )));
    }

    let needed = MEM_DATA_OFFSET
        .checked_add(requests.len() * MAX_MEM_OP)
        .ok_or_else(|| OpError::bad_request("MEM_BATCH ring size overflow"))?;
    let capacity = MEM_RING_BYTES.max(needed.next_power_of_two());

    if capacity > 4 * 1024 * 1024 {
        return Err(OpError::bad_request("MEM_BATCH ring exceeds 4 MiB"));
    }

    let mut ring = LockedBuffer::new(capacity)?;
    let buffer = ring.as_mut_slice();

    write_u32(buffer, 0, MEM_RING_MAGIC);
    write_u16(buffer, 4, 1);
    write_u16(buffer, 6, 0);
    write_u32(buffer, 8, capacity as u32);
    write_u16(buffer, 12, MEM_ENTRY_CAPACITY);
    write_u16(buffer, 14, 0);
    write_u16(buffer, 16, requests.len() as u16);
    write_u16(buffer, 18, 0);
    write_u32(buffer, 20, 1);
    write_u32(buffer, 24, 0);

    for (index, request) in requests.iter().enumerate() {
        if request.length as usize > MAX_MEM_OP {
            return Err(OpError::bad_request("MEM_BATCH entry is too large"));
        }

        let entry = MEM_ENTRY_OFFSET + index * 32;
        let data = MEM_DATA_OFFSET + index * MAX_MEM_OP;
        let length = request.payload.map_or(request.length, |payload| {
            payload.len().min(request.length as usize) as u32
        });

        write_u16(buffer, entry, request.operation);
        write_u16(buffer, entry + 2, 0);
        write_u32(buffer, entry + 4, length);
        write_u64(buffer, entry + 8, request.cr3);
        write_u64(buffer, entry + 16, request.address);
        write_u32(buffer, entry + 24, data as u32);
        write_u32(buffer, entry + 28, 0);

        if let Some(payload) = request.payload {
            if payload.len() > request.length as usize {
                return Err(OpError::bad_request("payload exceeds entry length"));
            }
            buffer[data..data + payload.len()].copy_from_slice(payload);
        }
    }

    Ok((ring, capacity))
}

fn run_mem_batch(
    config: &Config,
    args: &Map<String, Value>,
    requests: &[MemRequest<'_>],
) -> Result<(LockedBuffer, u64), OpError> {
    let (mut ring, capacity) = build_mem_ring(requests)?;
    let result = invoke(
        config,
        MEM_BATCH,
        args,
        ring.as_mut_slice().as_mut_ptr() as u64,
        capacity as u64,
        0,
    )?;
    let processed = result.rax;

    if processed != requests.len() as u64 {
        let ring_error = read_u32(ring.as_slice(), 24);
        let head = read_u16(ring.as_slice(), 14);

        return Err(OpError::unavailable(format!(
            "MEM_BATCH did not execute: processed={processed} expected={} head={head} ring_error={ring_error}",
            requests.len()
        )));
    }

    Ok((ring, processed))
}

fn ring_status(ring: &[u8]) -> u16 {
    u16::from_le_bytes([ring[MEM_ENTRY_OFFSET + 2], ring[MEM_ENTRY_OFFSET + 3]])
}

fn write_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(buffer: &mut [u8], offset: usize, value: u64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(buffer: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(buffer[offset..offset + 2].try_into().unwrap())
}

fn read_u32(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap())
}

fn read_u64(buffer: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap())
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2 + 2);
    text.push_str("0x");
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn hex(value: u64) -> String {
    format!("0x{value:X}")
}

fn mem_error_name(status: u16) -> &'static str {
    match status {
        0 => "ok",
        1 => "not_present",
        2 => "non_canonical",
        3 => "out_of_map",
        4 => "bad_cr3",
        5 => "not_ram",
        6 => "too_large",
        7 => "bad_entry",
        8 => "busy",
        9 => "host_fault",
        10 => "unsupported",
        11 => "hooked",
        12 => "unaligned",
        _ => "unknown",
    }
}

fn number_or_string(description: &str) -> Value {
    json!({
        "oneOf": [
            { "type": "integer", "minimum": 0 },
            { "type": "string" }
        ],
        "description": description
    })
}

fn processor_property() -> Value {
    number_or_string("CPU to pin to; use \"any\" to keep the current CPU.")
}

fn schema_with(properties: BTreeMap<&'static str, Value>, required: &[&str]) -> Value {
    let properties: Map<String, Value> = properties
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn empty_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert("processor", processor_property());

    schema_with(properties, &[])
}

fn ping_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert("nonce", number_or_string("Ping nonce, default 0."));
    properties.insert("processor", processor_property());

    schema_with(properties, &[])
}

fn flush_logs_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert(
        "capacity",
        number_or_string("Destination buffer size in bytes; default 65536."),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &[])
}

fn read_self_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert("page", number_or_string("Image page index, default 0."));
    properties.insert(
        "view",
        json!({
            "type": "string",
            "enum": ["root", "ept"],
            "description": "root = direct map, ept = guest EPT view."
        }),
    );
    properties.insert(
        "length",
        number_or_string("Bytes to return, capped at 4096; default 4096."),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &[])
}

fn mem_map_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert(
        "capacity",
        number_or_string("Maximum physical ranges, default 64."),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &[])
}

fn mem_read_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert("cr3", number_or_string("Guest CR3 used for translation."));
    properties.insert("address", number_or_string("Guest virtual address."));
    properties.insert(
        "length",
        number_or_string("Bytes to read, 1..=4096; default 4096."),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &["cr3", "address"])
}

fn mem_read_many_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert(
        "requests",
        json!({
            "type": "array",
            "minItems": 1,
            "maxItems": MAX_MEM_OPS,
            "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "cr3": number_or_string("Guest CR3 used for translation."),
                    "address": number_or_string("Guest virtual address."),
                    "length": number_or_string("Bytes to read, 1..=4096; default 4096.")
                },
                "required": ["cr3", "address"]
            }
        }),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &["requests"])
}

fn ntos_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert(
        "ntos_base",
        number_or_string("Optional ntoskrnl base VA; otherwise ExtraData/local lookup."),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &[])
}

fn ntos_process_modules_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert("pid", number_or_string("Target process ID."));
    properties.insert(
        "ntos_base",
        number_or_string("Optional ntoskrnl base VA; otherwise ExtraData/local lookup."),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &["pid"])
}

fn extra_data_set_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert(
        "value",
        number_or_string("Opaque 8-byte value to store in the hypervisor."),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &["value"])
}

fn mem_write_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert("cr3", number_or_string("Guest CR3 used for translation."));
    properties.insert("address", number_or_string("Guest virtual address."));
    properties.insert(
        "data",
        json!({
            "type": "string",
            "description": "Hex bytes to write, 1..=4096 bytes; 0x prefix optional."
        }),
    );
    properties.insert("processor", processor_property());

    schema_with(properties, &["cr3", "address", "data"])
}

fn mem_translate_schema() -> Value {
    let mut properties = BTreeMap::new();
    properties.insert("cr3", number_or_string("Guest CR3 used for translation."));
    properties.insert("address", number_or_string("Guest virtual address."));
    properties.insert("processor", processor_property());

    schema_with(properties, &["cr3", "address"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_ring_header_matches_hypervisor_abi() {
        let payload = [0xAAu8, 0xBB];
        let (ring, capacity) = build_mem_ring(&[MemRequest {
            operation: MEM_OP_WRITE_VIRT,
            cr3: 0x1ae000,
            address: 0xfffff800_00000000,
            length: payload.len() as u32,
            payload: Some(&payload),
        }])
        .unwrap();
        let ring = ring.as_slice();

        assert_eq!(capacity, 0x1_0000);
        assert_eq!(read_u32(ring, 0), MEM_RING_MAGIC);
        assert_eq!(read_u16(ring, 4), 1);
        assert_eq!(read_u32(ring, 8), capacity as u32);
        assert_eq!(read_u16(ring, 12), MEM_ENTRY_CAPACITY);
        assert_eq!(read_u16(ring, 14), 0);
        assert_eq!(read_u16(ring, 16), 1);
        assert_eq!(read_u32(ring, 20), 1);
        assert_eq!(read_u16(ring, MEM_ENTRY_OFFSET), MEM_OP_WRITE_VIRT);
        assert_eq!(read_u32(ring, MEM_ENTRY_OFFSET + 4), payload.len() as u32);
        assert_eq!(
            read_u32(ring, MEM_ENTRY_OFFSET + 24),
            MEM_DATA_OFFSET as u32
        );
        assert_eq!(&ring[MEM_DATA_OFFSET..MEM_DATA_OFFSET + 2], &payload);
    }

    #[test]
    fn batch_limit_is_one() {
        let err = build_mem_ring(&[
            MemRequest {
                operation: MEM_OP_READ_VIRT,
                cr3: 1,
                address: 2,
                length: 8,
                payload: None,
            },
            MemRequest {
                operation: MEM_OP_READ_VIRT,
                cr3: 3,
                address: 4,
                length: 16,
                payload: None,
            },
        ])
        .unwrap_err();
        assert_eq!(err.status, 400);
    }
}
