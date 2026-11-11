//! MCP JSON-RPC dispatcher. Tools mirror the named REST operations.

use serde_json::{Value, json};

use crate::{
    Config,
    operations::{self, OpError},
};

const PROTOCOL_VERSION: &str = "2025-03-26";

pub fn handle(body: &[u8], config: &Config) -> Option<Vec<u8>> {
    let request: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(error) => {
            return Some(encode(&rpc_error(
                Value::Null,
                -32700,
                &format!("invalid JSON: {error}"),
            )));
        }
    };

    match request {
        Value::Array(requests) => {
            let mut responses = Vec::new();

            for request in requests {
                if let Some(response) = handle_one(request, config) {
                    responses.push(response);
                }
            }

            if responses.is_empty() {
                None
            } else {
                Some(encode(&Value::Array(responses)))
            }
        }
        request => handle_one(request, config).map(|response| encode(&response)),
    }
}

fn handle_one(request: Value, config: &Config) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    if id.is_none() && method.starts_with("notifications/") {
        return None;
    }

    let id = id.unwrap_or(Value::Null);

    let response = match method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": "noumen-hypercall",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Named hypercall operations over REST and MCP. The server only issues VMCALL/VMMCALL; it never opens a driver or device."
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({
            "tools": operations::all()
                .iter()
                .filter(|operation| config.is_enabled(operation.name))
                .map(tool_definition)
                .collect::<Vec<_>>()
        })),
        "tools/call" => call_tool(&request, config),
        _ => Err((-32601, format!("method not found: {method}"))),
    };

    Some(match response {
        Ok(result) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }),
        Err((code, message)) => rpc_error(id, code, &message),
    })
}

fn tool_definition(operation: &operations::Operation) -> Value {
    json!({
        "name": operation.name,
        "title": operation.name.replace('_', " "),
        "description": format!(
            "{} REST: {} {}",
            operation.summary,
            operation.method.as_str(),
            operation.path
        ),
        "inputSchema": (operation.schema)(),
        "annotations": {
            "readOnlyHint": operation.method == operations::Method::Get,
            "destructiveHint": false,
            "idempotentHint": false,
            "openWorldHint": false
        }
    })
}

fn call_tool(request: &Value, config: &Config) -> Result<Value, (i64, String)> {
    let params = request
        .get("params")
        .and_then(Value::as_object)
        .ok_or((-32602, "tools/call requires params".to_owned()))?;

    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call requires params.name".to_owned()))?;

    let operation = operations::find(name).ok_or((-32602, format!("unknown operation: {name}")))?;

    if !config.is_enabled(operation.name) {
        return Err((-32602, format!("operation is disabled: {name}")));
    }

    let arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let value = (operation.handler)(config, &arguments).map_err(|error: OpError| {
        let code = if error.is_bad_request() {
            -32602
        } else {
            -32603
        };
        (code, error.message)
    })?;

    let text = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|_| "{\"error\":\"could not encode result\"}".to_owned());

    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": false
    }))
}

fn encode(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec())
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}
