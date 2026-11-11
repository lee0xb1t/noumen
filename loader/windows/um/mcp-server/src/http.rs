//! Tiny HTTP/1.1 server for the REST API and MCP Streamable HTTP endpoint.
//!
//! One request per connection; no TLS, sessions, or SSE stream.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use serde_json::{Map, Value, json};

use crate::{Config, mcp, operations};

const MAX_BODY: usize = 1024 * 1024;

pub fn serve(config: Arc<Config>) -> io::Result<()> {
    let listener = TcpListener::bind((config.bind, config.port))?;
    let address = listener.local_addr()?;

    println!("mcp-server listening on http://{address}/");
    println!("  Help: GET http://{address}/help   (all endpoints, params, examples)");
    println!("        or run `mcp-server --help`");
    println!("  REST endpoints:");
    for operation in operations::all()
        .iter()
        .filter(|operation| config.is_enabled(operation.name))
    {
        println!(
            "    {:<4} {:<26} {}",
            operation.method.as_str(),
            operation.path,
            operation.name
        );
    }
    println!("  Discovery: GET http://{address}/api");
    println!("  Health:    GET http://{address}/health");
    println!("  MCP:       POST http://{address}/mcp  (JSON-RPC for MCP clients; not REST)");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let config = Arc::clone(&config);
                thread::spawn(move || {
                    if let Err(error) = handle_client(stream, &config) {
                        eprintln!("mcp-server: connection error: {error}");
                    }
                });
            }
            Err(error) => eprintln!("mcp-server: accept error: {error}"),
        }
    }

    Ok(())
}

fn handle_client(stream: TcpStream, config: &Config) -> io::Result<()> {
    let mut stream = stream;
    let mut reader = BufReader::new(&stream);

    let Some(request) = read_request(&mut reader)? else {
        return Ok(());
    };

    drop(reader);

    let response = route(request, config);
    write_response(&mut stream, response)
}

struct Request {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
    reject: Option<u16>,
}

fn read_request(reader: &mut BufReader<&TcpStream>) -> io::Result<Option<Request>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let target = parts.next().unwrap_or("/");
    let (path, query) = target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query));

    if method.is_empty() {
        return Ok(None);
    }

    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        if header == "\r\n" || header == "\n" {
            break;
        }

        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }

    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid Content-Length: {error}"),
            )
        })?
        .unwrap_or(0);

    if content_length > MAX_BODY {
        return Ok(Some(Request {
            method,
            path: path.to_owned(),
            query: query.to_owned(),
            body: Vec::new(),
            reject: Some(413),
        }));
    }

    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return Ok(Some(Request {
            method,
            path: path.to_owned(),
            query: query.to_owned(),
            body: Vec::new(),
            reject: Some(411),
        }));
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    Ok(Some(Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        body,
        reject: None,
    }))
}

fn help_json(config: &Config) -> Value {
    let endpoints: Vec<_> = operations::all()
        .iter()
        .filter(|operation| config.is_enabled(operation.name))
        .map(|operation| {
            json!({
                "name": operation.name,
                "method": operation.method.as_str(),
                "path": operation.path,
                "summary": operation.summary,
                "inputSchema": (operation.schema)()
            })
        })
        .collect();

    let tools: Vec<_> = operations::all()
        .iter()
        .filter(|operation| config.is_enabled(operation.name))
        .map(|operation| operation.name)
        .collect();

    json!({
        "usage": "REST endpoints are listed in `endpoints`; MCP tools use the same names at POST /mcp.",
        "discovery": {
            "rest": "/api",
            "help": "/help",
            "health": "/health",
            "mcp": "/mcp"
        },
        "endpoints": endpoints,
        "mcp": {
            "endpoint": "/mcp",
            "transport": "JSON-RPC 2.0 over HTTP",
            "tools": tools
        },
        "examples": [
            "curl http://127.0.0.1:8000/api",
            "curl http://127.0.0.1:8000/help",
            "curl \"http://127.0.0.1:8000/api/ping?nonce=0x1234\"",
            "curl \"http://127.0.0.1:8000/api/memory/read?cr3=0x1ae000&address=0xfffff80000000000&length=16\""
        ]
    })
}

fn route(request: Request, config: &Config) -> Response {
    if request.method == "OPTIONS" {
        return Response::empty(204);
    }

    if let Some(status) = request.reject {
        let message = if status == 413 {
            "request body is too large"
        } else {
            "chunked requests are not supported"
        };

        return Response::json(status, &json!({ "error": message }));
    }

    if request.method == "GET" && (request.path == "/health" || request.path == "/") {
        let enabled: Vec<_> = operations::all()
            .iter()
            .filter(|operation| config.is_enabled(operation.name))
            .map(|operation| operation.name)
            .collect();

        return Response::json(
            200,
            &json!({
                "ok": true,
                "server": "noumen-hypercall",
                "operations": enabled
            }),
        );
    }

    if request.path == "/api" && request.method == "GET" {
        let operations: Vec<_> = operations::all()
            .iter()
            .filter(|operation| config.is_enabled(operation.name))
            .map(|operation| {
                json!({
                    "name": operation.name,
                    "method": operation.method.as_str(),
                    "path": operation.path,
                    "summary": operation.summary
                })
            })
            .collect();

        return Response::json(200, &json!({ "operations": operations }));
    }

    if request.path == "/help" && request.method == "GET" {
        return Response::json(200, &help_json(config));
    }

    if request.path == "/mcp" {
        return match request.method.as_str() {
            "POST" => match mcp::handle(&request.body, config) {
                Some(body) => Response::new(200, "application/json", body),
                None => Response::empty(202),
            },
            "GET" => Response::json(405, &json!({ "error": "use POST /mcp for JSON-RPC" })),
            "DELETE" => Response::empty(204),
            _ => Response::json(405, &json!({ "error": "method not allowed" })),
        };
    }

    let Some(operation) = operations::find_path(&request.path) else {
        return Response::json(404, &json!({ "error": "not found" }));
    };

    if !config.is_enabled(operation.name) {
        return Response::json(404, &json!({ "error": "operation is disabled" }));
    }

    if !operation.method.matches(&request.method) {
        return Response::json(
            405,
            &json!({
                "error": "method not allowed",
                "allowed": operation.method.as_str()
            }),
        );
    }

    let arguments = match request_arguments(operation.method, &request.query, &request.body) {
        Ok(arguments) => arguments,
        Err(message) => return Response::json(400, &json!({ "error": message })),
    };

    match (operation.handler)(config, &arguments) {
        Ok(value) => Response::json(200, &value),
        Err(error) => Response::json(error.status, &json!({ "error": error.message })),
    }
}

fn request_arguments(
    method: operations::Method,
    query: &str,
    body: &[u8],
) -> Result<Map<String, Value>, String> {
    let mut arguments = parse_query(query);

    if method == operations::Method::Post && !body.is_empty() {
        let value: Value =
            serde_json::from_slice(body).map_err(|error| format!("invalid JSON body: {error}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| "JSON body must be an object".to_owned())?;

        for (key, value) in object {
            arguments.insert(key.clone(), value.clone());
        }
    }

    Ok(arguments)
}

fn parse_query(query: &str) -> Map<String, Value> {
    let mut arguments = Map::new();

    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        arguments.insert(percent_decode(key), Value::String(percent_decode(value)));
    }

    arguments
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                if let (Some(high), Some(low)) =
                    (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
                {
                    decoded.push((high << 4) | low);
                    index += 3;
                } else {
                    decoded.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type,
            body,
        }
    }

    fn json(status: u16, value: &Value) -> Self {
        Self::new(
            status,
            "application/json",
            serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()),
        )
    }

    fn empty(status: u16) -> Self {
        Self::new(status, "application/json", Vec::new())
    }
}

fn write_response(stream: &mut TcpStream, response: Response) -> io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Response",
    };

    let headers = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, DELETE, OPTIONS\r\n\
         Access-Control-Allow-Headers: content-type, accept, mcp-session-id, mcp-protocol-version\r\n\
         \r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );

    stream.write_all(headers.as_bytes())?;
    if !response.body.is_empty() {
        stream.write_all(&response.body)?;
    }
    stream.flush()
}
