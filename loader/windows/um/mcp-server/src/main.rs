//! Named REST/MCP bridge to noumen hypercalls.
//!
//! No loader, driver, IOCTL, or device object is involved. Each operation runs
//! `VMCALL` (Intel) or `VMMCALL` (AMD) from CPL3 with the Noumen register ABI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "console")]

mod generated {
    include!(concat!(env!("OUT_DIR"), "/hypercall_key.rs"));
}

mod http;
mod hypercall;
mod mcp;
mod operations;

use std::env;
use std::net::IpAddr;
use std::process;
use std::sync::Arc;

pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    pub key: u64,
    pub processor: Option<u32>,
    pub only: Option<Vec<&'static str>>,
}

impl Config {
    pub fn is_enabled(&self, name: &str) -> bool {
        self.only.as_ref().is_none_or(|names| names.contains(&name))
    }
}

fn main() {
    let config = match parse_args() {
        Ok(Some(config)) => config,
        Ok(None) => return,
        Err(error) => {
            eprintln!("mcp-server: {error}");
            eprintln!();
            print_usage();
            process::exit(2);
        }
    };

    if let Err(error) = http::serve(Arc::new(config)) {
        eprintln!("mcp-server: {error}");
        process::exit(1);
    }
}

fn parse_args() -> Result<Option<Config>, String> {
    let mut bind: IpAddr = "127.0.0.1".parse().unwrap();
    let mut port = 8000u16;
    let mut key = generated::DEFAULT_KEY;
    let mut processor = Some(0u32);
    let mut only: Option<Vec<&'static str>> = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => {
                let value = args.next().ok_or("--bind requires a value")?;
                bind = value
                    .parse()
                    .map_err(|error| format!("invalid --bind value {value:?}: {error}"))?;
            }
            "--port" => {
                let value = args.next().ok_or("--port requires a value")?;
                port = value
                    .parse()
                    .map_err(|error| format!("invalid --port value {value:?}: {error}"))?;
            }
            "--key" => {
                let value = args.next().ok_or("--key requires a value")?;
                key = parse_u64(&value).map_err(|error| format!("invalid --key: {error}"))?;
            }
            "--processor" => {
                let value = args.next().ok_or("--processor requires a value")?;
                processor = if value.eq_ignore_ascii_case("any") {
                    None
                } else {
                    Some(
                        value
                            .parse::<u32>()
                            .map_err(|error| format!("invalid --processor: {error}"))?,
                    )
                };
            }
            "--only" => {
                let value = args.next().ok_or("--only requires operation names")?;
                let selected = only.get_or_insert_with(Vec::new);

                for name in value
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    let operation = operations::find(name)
                        .ok_or_else(|| format!("unknown operation: {name}"))?;

                    if !selected.contains(&operation.name) {
                        selected.push(operation.name);
                    }
                }
            }
            "--list-operations" => {
                print_operations();
                return Ok(None);
            }
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Some(Config {
        bind,
        port,
        key,
        processor,
        only,
    }))
}

fn parse_u64(text: &str) -> Result<u64, std::num::ParseIntError> {
    match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    }
}

fn print_operations() {
    println!("Available operations:");
    for operation in operations::all() {
        println!(
            "  {:<16} {:<5} {}  {}",
            operation.name,
            operation.method.as_str(),
            operation.path,
            operation.summary
        );
    }
}

fn print_help() {
    print_usage();

    println!();
    println!("REST endpoints:");
    for operation in operations::all() {
        println!(
            "  {:<4} {:<26} {:<14} params: {}",
            operation.method.as_str(),
            operation.path,
            operation.name,
            operations::parameters_text(operation)
        );
        println!("       {}", operation.summary);
    }

    println!();
    println!("MCP: POST /mcp");
    println!(
        "  tools: {}",
        operations::all()
            .iter()
            .map(|operation| operation.name)
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  The tool names are the same as the REST operation names.");
    println!("  Required parameters are marked with * above.");

    println!();
    println!("Examples:");
    println!("  curl.exe http://127.0.0.1:8000/api");
    println!("  curl.exe http://127.0.0.1:8000/help");
    println!("  curl.exe \"http://127.0.0.1:8000/api/ping?nonce=0x1234\"");
    println!(
        "  curl.exe \"http://127.0.0.1:8000/api/memory/read?cr3=0x1ae000&address=0xfffff80000000000&length=16\""
    );
    println!(
        "  curl.exe -X POST http://127.0.0.1:8000/api/memory/write -H \"Content-Type: application/json\" -d '{{\"cr3\":\"0x1ae000\",\"address\":\"0xfffff80000000000\",\"data\":\"DEADBEEF\"}}'"
    );
    println!("  MCP tools/call: {{\"name\":\"ping\",\"arguments\":{{\"nonce\":\"0x1234\"}}}}");
}

fn print_usage() {
    println!(
        r#"noumen named hypercall REST/MCP server

Usage: mcp-server [options]

Options:
  --bind <addr>          listen address (default 127.0.0.1)
  --port <port>          HTTP port (default 8000)
  --key <u64>            hypercall key (default built-in 0x{key:X})
  --processor <n|any>    pin calls to CPU n (default 0); any = current CPU
  --only <names>         expose only comma-separated operation names
  --list-operations      print operation names, methods and paths
  -h, --help             show this help

REST discovery: GET  http://<addr>:<port>/api
REST help:      GET  http://<addr>:<port>/help
MCP endpoint:   POST http://<addr>:<port>/mcp
Health check:   GET  http://<addr>:<port>/health"#,
        key = generated::DEFAULT_KEY
    );
}
