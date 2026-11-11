//! Smoke-test a running (or freshly launched) noumen MCP/REST server.
//!
//! Without `--call` this tool never executes a hypercall. It validates
//! /health, /api, /help, MCP initialize/tools/list and the named operation
//! mapping shared by REST and MCP.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transport {
    Mcp,
    Rest,
    Both,
}

impl Transport {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "mcp" => Ok(Self::Mcp),
            "rest" => Ok(Self::Rest),
            "both" => Ok(Self::Both),
            _ => Err(format!("invalid --transport: {value}")),
        }
    }
}

struct Options {
    url: String,
    exe: Option<String>,
    server_args: Vec<String>,
    timeout: Duration,
    call: Option<String>,
    args: Vec<String>,
    transport: Transport,
}

impl Options {
    fn parse() -> Result<Option<Self>> {
        let mut options = Self {
            url: "http://127.0.0.1:8000".to_owned(),
            exe: None,
            server_args: Vec::new(),
            timeout: Duration::from_secs(5),
            call: None,
            args: Vec::new(),
            transport: Transport::Mcp,
        };
        let mut explicit_url = false;

        let mut args = env::args().skip(1);
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--url" => {
                    options.url = args.next().ok_or("--url requires a value")?;
                    explicit_url = true;
                }
                "--exe" => {
                    options.exe = Some(args.next().ok_or("--exe requires a value")?);
                }
                "--server-arg" => {
                    options
                        .server_args
                        .push(args.next().ok_or("--server-arg requires a value")?);
                }
                "--timeout" => {
                    let value = args.next().ok_or("--timeout requires a value")?;
                    let seconds: f64 = value
                        .parse()
                        .map_err(|error| format!("invalid --timeout: {error}"))?;
                    if !seconds.is_finite() || seconds <= 0.0 {
                        return Err("--timeout must be positive".to_owned());
                    }
                    options.timeout = Duration::from_secs_f64(seconds);
                }
                "--call" => {
                    options.call = Some(args.next().ok_or("--call requires a value")?);
                }
                "--arg" => {
                    options
                        .args
                        .push(args.next().ok_or("--arg requires key=value")?);
                }
                "--transport" => {
                    let value = args.next().ok_or("--transport requires a value")?;
                    options.transport = Transport::parse(&value)?;
                }
                "-h" | "--help" => {
                    print_usage();
                    return Ok(None);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }

        if explicit_url && options.exe.is_some() {
            return Err("--url and --exe are mutually exclusive".to_owned());
        }

        Ok(Some(options))
    }
}

fn main() {
    let options = match Options::parse() {
        Ok(Some(options)) => options,
        Ok(None) => return,
        Err(error) => {
            eprintln!("FAIL  {error}");
            eprintln!();
            print_usage();
            std::process::exit(2);
        }
    };

    if let Err(error) = run(options) {
        eprintln!("FAIL  {error}");
        std::process::exit(1);
    }
}

fn run(options: Options) -> Result<()> {
    let mut server: Option<Child> = None;
    let base_url = if let Some(exe) = &options.exe {
        let port = free_port()?;
        let mut command = Command::new(exe);
        command
            .arg("--port")
            .arg(port.to_string())
            .args(&options.server_args)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        server = Some(
            command
                .spawn()
                .map_err(|error| format!("could not launch {exe}: {error}"))?,
        );
        format!("http://127.0.0.1:{port}")
    } else {
        options.url.clone()
    };

    let result = (|| {
        let client = Client::new(&base_url, options.timeout)?;
        wait_health(&client, options.timeout)?;

        let health_operations = check_health(&client)?;
        let api = fetch_api(&client)?;
        if health_operations != api.keys().cloned().collect::<BTreeSet<_>>() {
            return Err("/health and /api operation sets differ".to_owned());
        }

        check_help(&client, &api)?;
        check_mcp(&client, &api)?;

        if let Some(name) = &options.call {
            let arguments = parse_arguments(&options.args)?;
            call_operation(&client, &api, name, &arguments, options.transport)?;
        } else {
            println!("SKIP  no --call: hypercall was not executed");
        }

        println!("OK");
        Ok(())
    })();

    if let Some(mut child) = server {
        let _ = child.kill();
        let _ = child.wait();
    }

    result
}

fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("could not allocate a local port: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("could not read local port: {error}"))?
        .port();

    Ok(port)
}

struct Client {
    host: String,
    port: u16,
    timeout: Duration,
}

impl Client {
    fn new(base_url: &str, timeout: Duration) -> Result<Self> {
        let rest = base_url
            .strip_prefix("http://")
            .ok_or_else(|| format!("only http:// URLs are supported: {base_url}"))?;
        let authority = rest.split('/').next().unwrap_or(rest);
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or_else(|| format!("URL must include host:port: {base_url}"))?;
        let port = port
            .parse()
            .map_err(|error| format!("invalid port in {base_url}: {error}"))?;

        Ok(Self {
            host: host.to_owned(),
            port,
            timeout,
        })
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
        query: &[(String, String)],
    ) -> Result<(u16, Option<Value>)> {
        let mut target = path.to_owned();
        if !query.is_empty() {
            target.push('?');
            target.push_str(
                &query
                    .iter()
                    .map(|(key, value)| {
                        format!("{}={}", percent_encode(key), percent_encode(value))
                    })
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        }

        let body = payload
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|error| format!("could not encode request body: {error}"))?
            .unwrap_or_default();

        let mut request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/json\r\nConnection: close\r\n",
            self.host, self.port
        );
        if !body.is_empty() {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");

        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|error| format!("connect {}:{}: {error}", self.host, self.port))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| format!("set read timeout: {error}"))?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|error| format!("set write timeout: {error}"))?;

        stream
            .write_all(request.as_bytes())
            .and_then(|_| stream.write_all(&body))
            .map_err(|error| format!("send request: {error}"))?;
        let _ = stream.shutdown(Shutdown::Write);

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .map_err(|error| format!("read response: {error}"))?;

        parse_response(&response)
    }

    fn mcp(&self, method: &str, params: Option<Value>, notification: bool) -> Result<Value> {
        let mut payload = Map::new();
        payload.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
        payload.insert("method".to_owned(), Value::String(method.to_owned()));
        if !notification {
            payload.insert("id".to_owned(), Value::from(1));
        }
        if let Some(params) = params {
            payload.insert("params".to_owned(), params);
        }

        let (status, body) = self.request("POST", "/mcp", Some(&Value::Object(payload)), &[])?;
        if status != 200 {
            return Err(format!("MCP {method}: HTTP {status}: {body:?}"));
        }

        body.ok_or_else(|| format!("MCP {method}: empty response"))
    }
}

fn parse_response(response: &[u8]) -> Result<(u16, Option<Value>)> {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("HTTP response has no header terminator")?;
    let headers = String::from_utf8_lossy(&response[..separator]);
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or("HTTP response has no status")?
        .parse()
        .map_err(|error| format!("invalid HTTP status: {error}"))?;
    let body = &response[separator + 4..];

    let parsed = if body.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice(body)
                .map_err(|error| format!("response is not JSON: {error}"))?,
        )
    };

    Ok((status, parsed))
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn check_health(client: &Client) -> Result<BTreeSet<String>> {
    let (status, body) = client.request("GET", "/health", None, &[])?;
    if status != 200 {
        return Err(format!("GET /health: HTTP {status}"));
    }
    let body = body.ok_or("GET /health: empty body")?;
    let object = body
        .as_object()
        .ok_or("GET /health: body is not an object")?;
    if object.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("GET /health: ok is not true".to_owned());
    }
    let operations = object
        .get("operations")
        .and_then(Value::as_array)
        .ok_or("GET /health: operations is not a list")?;

    let mut names = BTreeSet::new();
    for name in operations {
        names.insert(
            name.as_str()
                .ok_or("GET /health: bad operation name")?
                .to_owned(),
        );
    }

    println!("PASS  /health: {} operations", names.len());
    Ok(names)
}

#[derive(Clone)]
struct Operation {
    method: String,
    path: String,
}

fn fetch_api(client: &Client) -> Result<BTreeMap<String, Operation>> {
    let (status, body) = client.request("GET", "/api", None, &[])?;
    if status != 200 {
        return Err(format!("GET /api: HTTP {status}"));
    }
    let body = body.ok_or("GET /api: empty body")?;
    let operations = body
        .as_object()
        .and_then(|object| object.get("operations"))
        .and_then(Value::as_array)
        .ok_or("GET /api: operations is not a list")?;

    let mut result = BTreeMap::new();
    for operation in operations {
        let object = operation
            .as_object()
            .ok_or("GET /api: operation is not an object")?;
        let name = string_field(object, "name")?;
        let method = string_field(object, "method")?;
        let path = string_field(object, "path")?;
        let _summary = string_field(object, "summary")?;

        if result.contains_key(&name) {
            return Err(format!("GET /api: duplicate operation {name}"));
        }
        if method != "GET" && method != "POST" {
            return Err(format!("GET /api: bad method for {name}"));
        }
        if !path.starts_with("/api/") {
            return Err(format!("GET /api: bad path for {name}"));
        }

        result.insert(name, Operation { method, path });
    }

    if result.is_empty() {
        return Err("GET /api: no operations".to_owned());
    }

    println!("PASS  /api: {} named operations", result.len());
    Ok(result)
}

fn check_help(client: &Client, api: &BTreeMap<String, Operation>) -> Result<()> {
    let (status, body) = client.request("GET", "/help", None, &[])?;
    if status != 200 {
        return Err(format!("GET /help: HTTP {status}"));
    }
    let body = body.ok_or("GET /help: empty body")?;
    let object = body.as_object().ok_or("GET /help: body is not an object")?;
    let endpoints = object
        .get("endpoints")
        .and_then(Value::as_array)
        .ok_or("GET /help: endpoints is not a list")?;

    let mut help = BTreeMap::new();
    for endpoint in endpoints {
        let endpoint = endpoint
            .as_object()
            .ok_or("GET /help: endpoint is not an object")?;
        let name = string_field(endpoint, "name")?;
        let method = string_field(endpoint, "method")?;
        let path = string_field(endpoint, "path")?;
        if endpoint
            .get("inputSchema")
            .and_then(Value::as_object)
            .is_none()
        {
            return Err(format!("GET /help: missing schema for {name}"));
        }
        help.insert(name, (method, path));
    }

    if help.keys().collect::<BTreeSet<_>>() != api.keys().collect::<BTreeSet<_>>() {
        return Err("GET /help: operation set differs from /api".to_owned());
    }

    for (name, operation) in api {
        let (method, path) = help
            .get(name)
            .ok_or_else(|| format!("GET /help: missing {name}"))?;
        if method != &operation.method || path != &operation.path {
            return Err(format!("GET /help: method/path differs for {name}"));
        }
    }

    let mcp = object
        .get("mcp")
        .and_then(Value::as_object)
        .ok_or("GET /help: mcp is not an object")?;
    let tools = mcp
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("GET /help: mcp.tools is not a list")?;
    let tool_names: BTreeSet<_> = tools
        .iter()
        .map(|tool| {
            tool.as_str()
                .ok_or_else(|| "GET /help: bad MCP tool name".to_owned())
        })
        .collect::<Result<_>>()?;

    if tool_names != api.keys().map(String::as_str).collect::<BTreeSet<_>>() {
        return Err("GET /help: MCP tools differ from REST operations".to_owned());
    }

    println!("PASS  /help: REST and MCP operations match");
    Ok(())
}

fn check_mcp(client: &Client, api: &BTreeMap<String, Operation>) -> Result<()> {
    let initialized = client.mcp(
        "initialize",
        Some(json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test_mcp_server", "version": "0.1" }
        })),
        false,
    )?;
    let result = initialized
        .as_object()
        .and_then(|object| object.get("result"))
        .and_then(Value::as_object)
        .ok_or("MCP initialize: missing result")?;
    let protocol = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or("MCP initialize: missing protocolVersion")?;
    if result
        .get("serverInfo")
        .and_then(Value::as_object)
        .is_none()
    {
        return Err("MCP initialize: missing serverInfo".to_owned());
    }
    println!("PASS  MCP initialize: {protocol}");

    let (status, _) = client.request(
        "POST",
        "/mcp",
        Some(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        })),
        &[],
    )?;
    if status != 202 {
        return Err(format!("MCP notifications/initialized: HTTP {status}"));
    }
    println!("PASS  MCP notifications/initialized");

    let listed = client.mcp("tools/list", Some(json!({})), false)?;
    let tools = listed
        .as_object()
        .and_then(|object| object.get("result"))
        .and_then(Value::as_object)
        .and_then(|result| result.get("tools"))
        .and_then(Value::as_array)
        .ok_or("MCP tools/list: missing tools")?;

    let mut names = BTreeSet::new();
    for tool in tools {
        let tool = tool
            .as_object()
            .ok_or("MCP tools/list: tool is not an object")?;
        let name = string_field(tool, "name")?;
        if !api.contains_key(&name) {
            return Err(format!("MCP tools/list: unknown tool {name}"));
        }
        if !names.insert(name.clone()) {
            return Err(format!("MCP tools/list: duplicate tool {name}"));
        }
        if name == "hypercall" {
            return Err("MCP tools/list: raw hypercall tool is present".to_owned());
        }

        let schema = tool
            .get("inputSchema")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("MCP tools/list: {name} has no inputSchema"))?;
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("MCP tools/list: {name} properties is bad"))?;
        if properties.contains_key("command") {
            return Err(format!("MCP tools/list: {name} exposes command"));
        }
        if properties.contains_key("arg0") {
            return Err(format!("MCP tools/list: {name} exposes arg0"));
        }
    }

    if names != api.keys().cloned().collect::<BTreeSet<_>>() {
        return Err("MCP tools/list: tool set differs from /api".to_owned());
    }

    println!("PASS  MCP tools/list: {} named tools", names.len());
    Ok(())
}

fn string_field(object: &Map<String, Value>, field: &str) -> Result<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing string field {field}"))
}

fn parse_arguments(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut arguments = BTreeMap::new();

    for value in values {
        let (key, raw) = value
            .split_once('=')
            .ok_or_else(|| format!("bad --arg {value:?}, expected key=value"))?;
        if key.is_empty() || raw.is_empty() {
            return Err(format!("bad --arg {value:?}, expected key=value"));
        }
        arguments.insert(key.to_owned(), raw.to_owned());
    }

    Ok(arguments)
}

fn call_operation(
    client: &Client,
    api: &BTreeMap<String, Operation>,
    name: &str,
    arguments: &BTreeMap<String, String>,
    transport: Transport,
) -> Result<()> {
    let operation = api
        .get(name)
        .ok_or_else(|| format!("unknown operation: {name}"))?;

    if transport == Transport::Mcp || transport == Transport::Both {
        let response = client.mcp(
            "tools/call",
            Some(json!({
                "name": name,
                "arguments": arguments
            })),
            false,
        )?;
        let result = response
            .as_object()
            .and_then(|object| object.get("result"))
            .and_then(Value::as_object)
            .ok_or_else(|| format!("MCP tools/call: {name} missing result"))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(format!("MCP tools/call: {name} returned isError"));
        }
        println!("PASS  MCP tools/call: {name}");
    }

    if transport == Transport::Rest || transport == Transport::Both {
        let (status, body) = if operation.method == "GET" {
            let query: Vec<_> = arguments
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            client.request("GET", &operation.path, None, &query)?
        } else {
            client.request(
                "POST",
                &operation.path,
                Some(&serde_json::to_value(arguments).map_err(|error| error.to_string())?),
                &[],
            )?
        };

        if status != 200 {
            return Err(format!("REST {name}: HTTP {status}: {body:?}"));
        }
        if !body.map(|value| value.is_object()).unwrap_or(false) {
            return Err(format!("REST {name}: body is not an object"));
        }
        println!("PASS  REST {} {}: {name}", operation.method, operation.path);
    }

    Ok(())
}

fn wait_health(client: &Client, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut last_error = String::new();

    while Instant::now() < deadline {
        match client.request("GET", "/health", None, &[]) {
            Ok((200, _)) => return Ok(()),
            Ok((status, _)) => last_error = format!("HTTP {status}"),
            Err(error) => last_error = error,
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    Err(format!("server did not become healthy: {last_error}"))
}

fn print_usage() {
    println!(
        r#"Smoke-test a noumen MCP/REST server.

Usage: test_mcp_server [options]

Options:
  --url <url>             server base URL (default http://127.0.0.1:8000)
  --exe <path>            launch mcp-server.exe on a free local port
  --server-arg <arg>      extra argument for --exe; repeatable
  --timeout <seconds>     HTTP/startup timeout (default 5)
  --call <operation>      invoke one named operation
  --arg <key=value>       argument for --call; repeatable
  --transport <mcp|rest|both>
                          transport for --call (default mcp)
  -h, --help              show this help

Without --call this tool never executes VMCALL.
"#
    );
}
