# MCP.md

noumen MCP/REST 服务的唯一说明入口。服务在 `loader/windows/um/mcp-server/`，
把 hypercall 包装成**具名 REST 资源**和**同名 MCP 工具**。

每个操作只执行一次 CPL3 `VMCALL`/`VMMCALL`。

## 两个端点

`/api/*` 和 `/mcp` 不是两套重复实现：

- `/api/*` 是普通 REST API，供 curl、浏览器、脚本或任意 HTTP 客户端使用；
- `/mcp` 是 **MCP 协议端点**，只接收 JSON-RPC 2.0（`initialize`、
  `tools/list`、`tools/call` 等），供 MCP 客户端使用；
- 两者共用 `operations.rs` 中同一套具名 handler，只是协议外壳不同。

`/mcp` 不是 REST 资源，不应使用 `GET /mcp` 或 REST 语义调用；`GET /mcp`
会返回 405。只想用普通 HTTP 时只使用 `/api/*` 即可。

## 构建

```powershell
# 随 kdmapper 构建，产物在 target/kdmapper/mcp-server.exe
python build_kdmapper.py

# 只重编用户态程序
python build_kdmapper.py --skip-hypervisor --skip-driver

# 单独构建
cd loader/windows/um/mcp-server
cargo build --release
```

默认 key 为 `0x10F2C`，可由 `python build_kdmapper.py --hypercall-key 0x2b3d`
或环境变量 `NOUMEN_HYPERCALL_KEY` 覆盖。

## 启动

```powershell
.\target\kdmapper\mcp-server.exe
```

默认监听 `127.0.0.1:8000`：

```text
REST:   http://127.0.0.1:8000/api
Help:   http://127.0.0.1:8000/help
Health: http://127.0.0.1:8000/health
MCP:    http://127.0.0.1:8000/mcp
```

启动时控制台会列出 `/help`、全部启用的 REST 接口，以及 `/api`、`/health` 和
MCP 端点；`GET /help` 和 `mcp-server --help` 会列出完整参数和示例。

| 参数 | 默认 | 说明 |
|---|---|---|
| `--bind` | `127.0.0.1` | 监听地址 |
| `--port` | `8000` | HTTP 端口 |
| `--key` | 构建期默认 | hypercall key |
| `--processor` | `0` | VMCALL 前绑定的 CPU；`any` = 当前 CPU |
| `--only` | 全部 | 只暴露逗号分隔的操作名 |
| `--list-operations` | - | 列出操作名、方法和路径 |
| `-h`, `--help` | - | 列出全部接口、参数和用法 |

`--processor 0` 对应 kdmapper 的 `Start(FALSE, FALSE)` 选出的
`StartProcessor = 0`。线程落到未虚拟化 CPU 时 `VMCALL` 会 `#UD` 并终止进程。

## 发现和帮助

```http
GET /api      # 已启用操作：name/method/path/summary
GET /help     # 全部接口、参数 schema、MCP 工具名和示例
GET /health   # 服务状态和操作名，不执行 hypercall
```

`mcp-server --help` 会列出同样的全部操作，包括方法、路径、参数（必需参数带 `*`）
和示例。

## REST API

约定：

- GET 用 query string，POST 用 JSON body；
- 数字可用十进制或 `0x` 十六进制字符串；
- 所有操作都接受可选的 `processor`，覆盖本次 CPU 绑定；
- 失败返回 JSON `{"error":"..."}`。

| 操作 | 方法 | 路径 | 参数 |
|---|---|---|---|
| `ping` | GET | `/api/ping` | `nonce?` |
| `logger_info` | GET | `/api/logs` | - |
| `flush_logs` | POST | `/api/logs/flush` | `capacity?` |
| `hide_self` | POST | `/api/self/hide` | - |
| `read_self` | GET | `/api/self/read` | `page?`, `view?`, `length?` |
| `mem_query` | GET | `/api/memory` | - |
| `mem_map` | GET | `/api/memory/map` | `capacity?` |
| `mem_read` | GET | `/api/memory/read` | `cr3*`, `address*`, `length?` |
| `mem_write` | POST | `/api/memory/write` | `cr3*`, `address*`, `data*` |
| `mem_translate` | GET | `/api/memory/translate` | `cr3*`, `address*` |
| `hooks_apply` | POST | `/api/hooks/apply` | - |
| `hooks_retired` | GET | `/api/hooks/retired` | - |

常用约束：

- `read_self.view` 为 `root` 或 `ept`，`length` 最大 4096；
- `flush_logs.capacity` 默认 65536，上限 1 MiB；每条日志记录 152 字节；
- `mem_map.capacity` 默认 64，上限 1024；
- `mem_read`/`mem_write` 长度 1–4096，`mem_write.data` 是十六进制字节串；
- `hide_self`、`mem_write`、`hooks_apply` 是管理面操作。

示例：

```powershell
curl.exe http://127.0.0.1:8000/api
curl.exe http://127.0.0.1:8000/help
curl.exe "http://127.0.0.1:8000/api/ping?nonce=0x1234"
curl.exe "http://127.0.0.1:8000/api/memory/read?cr3=0x1ae000&address=0xfffff80000000000&length=16"

curl.exe -X POST http://127.0.0.1:8000/api/memory/write `
  -H "Content-Type: application/json" `
  -d '{"cr3":"0x1ae000","address":"0xfffff80000000000","data":"DEADBEEF"}'
```

## MCP

端点：

```text
POST http://127.0.0.1:8000/mcp
```

JSON-RPC 2.0 over HTTP；无会话、无 SSE，`GET /mcp` 返回 405。支持
`initialize`、`notifications/initialized`、`ping`、`tools/list`、`tools/call`。
工具名与 REST 操作名完全一致，参数也一致；没有 `hypercall` 工具、`command`
编号或 `arg0/arg1/arg2`。

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "ping",
    "arguments": { "nonce": "0x1234" }
  }
}
```

MCP 结果包含 `content[0].text`（格式化 JSON）、`structuredContent`（同一对象）
和 `isError: false`。

`--only` 同时过滤 REST 和 MCP；被过滤的操作在 REST 返回 404，在 MCP 返回
`-32602 unknown operation`。

## 实现

```text
src/main.rs        CLI、Config、监听
src/http.rs        HTTP/1.1、REST 路由、/help、/mcp
src/mcp.rs         JSON-RPC、tools/list、tools/call
src/operations.rs  具名操作注册表、参数和 handler
src/hypercall.rs   唯一 VMCALL/VMMCALL ABI、AMD 选择、CPU 绑定
build.rs           NOUMEN_HYPERCALL_KEY -> 默认 key
```

REST 和 MCP 共用 `operations.rs` 的同一套 handler，不存在两套行为。

## 测试工具

`tools/test_mcp_server` 是 Rust 工具；kdmapper 构建会一并暂存为
`target/kdmapper/test_mcp_server.exe`。默认会通过 **MCP `tools/call`** 实际执行
一次 `ping`，并校验 `ok == true`、`reply == expected`：

```powershell
# 连接已有服务并执行真实 ping
.\target\kdmapper\test_mcp_server.exe --url http://127.0.0.1:8000

# 或自动启动 mcp-server.exe（随机本地端口）
.\target\kdmapper\test_mcp_server.exe --exe .\target\kdmapper\mcp-server.exe
```

检查 `/health`、`/api`、`/help`、MCP `initialize`、MCP `tools/list`，并确认 REST
操作与 MCP 工具名称、方法、路径一致，且没有 `hypercall`/`command` 接口。

`/health`、`/api`、`/help` 和 `tools/list` 只是服务/协议发现，不执行 VMCALL；
它们的 HTTP 200 只证明 HTTP/MCP 层正常。真正的 hypercall 验证来自默认的
`ping`，或显式 `--call`。

没有加载 hypervisor 时使用 `--structure-only`，它会跳过 VMCALL：

```powershell
.\target\kdmapper\test_mcp_server.exe --exe .\target\kdmapper\mcp-server.exe `
  --structure-only
```

自定义操作和传输方式：

```powershell
.\target\kdmapper\test_mcp_server.exe --exe .\target\kdmapper\mcp-server.exe `
  --call ping --arg nonce=0x1234 --transport both --verbose
```

`--transport` 可选 `mcp`、`rest`、`both`；`both` 会分别通过 MCP 和 REST 各执行
一次。工具在调用后校验 `ok`，内存操作则校验 `status == 0`，失败会打印 `FAIL`。

所有 HTTP 请求、响应和检查结果都会写入 `test_mcp_server.log`；可用 `--log`
指定其他路径：

```powershell
.\target\kdmapper\test_mcp_server.exe --exe .\target\kdmapper\mcp-server.exe `
  --log .\mcp-test.log
```

加 `--verbose`（或 `-v`）时，同一份完整 JSON 也会打印到控制台，包括
`/api` 操作字段、`/help` 的 `inputSchema`、MCP `tools/list` 的完整工具结构，
以及 `--call` 的 `structuredContent` 返回值：

```powershell
.\target\kdmapper\test_mcp_server.exe --exe .\target\kdmapper\mcp-server.exe --verbose
```

执行真实调用前需 noumen 已加载且当前 CPU 已虚拟化；否则会 `#UD` 并终止服务进程。

## 安全边界

- 默认只监听 loopback，没有 TLS、认证或访问控制；不要暴露到不可信网络；
- 使用前按 [README.md](README.md) 的“使用前注意”关闭 VBS、内存完整性/HVCI 和 Hyper-V；
- key 错误、hypervisor 未运行或 CPU 未虚拟化时，`VMCALL` 会 `#UD` 并终止进程；
- `--processor any` 只适合所有 CPU 都已虚拟化的多核模式；
- MEM_* 缓冲区是普通进程内存，hypervisor 通过当前 CR3 读取；页面被换出时可能失败；
- 未实现 `STOP`、hook 安装/移除、用户断点等待、`MEM_SCAN`、`MEM_STATS`；
- 宿主测试不覆盖真实 `VMCALL`，上机先用 `ping` 验证 key 和 CPU 绑定。

## 验证

```powershell
cd loader/windows/um/mcp-server
cargo build --release
cargo clippy --release
.\target\x86_64-pc-windows-msvc\release\mcp-server.exe --help

# Rust 冒烟测试工具（不会执行 VMCALL）
cd ..\..\..\..
.\target\kdmapper\test_mcp_server.exe --exe .\target\kdmapper\mcp-server.exe
```

无需 hypervisor 的检查：

```text
GET /api、GET /help、GET /health
MCP initialize / tools/list
缺少必需参数 -> 400
`--only` 同时过滤 REST/MCP
旧 `--hypercall` 参数和旧 `hypercall` 工具名被拒绝
```
