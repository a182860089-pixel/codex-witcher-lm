//! Rewrite Grok's Codex-app MCP guesses before Codex executes them.
//!
//! Official Codex models call `list_threads` / `read_thread` on server
//! `codex_app`. Grok often emits the generic `read_mcp_resource` tool with
//! plugin-id aliases (`codex-app-tools`, `codex-app`, …) or a `turnLimit`
//! above the plugin's max of 10. Those calls fail locally even though the
//! plugin itself is running.

use serde_json::{Value, json};

pub const MCP_COMPAT_INSTRUCTION: &str = "The Codex app MCP server is named \"codex_app\" (underscore). Call list_threads and read_thread in namespace mcp__codex_app. read_thread turnLimit must be <= 10. Never call read_mcp_resource; it cannot read Codex threads.";

const CODEX_APP_NAMESPACE: &str = "mcp__codex_app";
const READ_THREAD_MAX_TURN_LIMIT: u64 = 10;
const CODEX_APP_ALIASES: &[&str] = &[
    "codex_app",
    "codex-app",
    "codex-app-tools",
    "plugin-codex-app-tools-codex-app",
    "chatgpt app tools",
    "codex app",
];

#[derive(Debug, Default)]
pub struct McpRewriteBuffer {
    pending: Option<PendingReadMcp>,
}

#[derive(Debug)]
struct PendingReadMcp {
    events: Vec<Vec<u8>>,
    item: Value,
    arguments: String,
}

impl McpRewriteBuffer {
    pub fn ingest(&mut self, event: Vec<u8>) -> Vec<Vec<u8>> {
        if event_is_sse_comment(&event) {
            return vec![event];
        }
        if event_is_completed(&event) {
            let mut out = self.flush();
            out.push(rewrite_sse_event(&event));
            return out;
        }
        let Some(mut json) = sse_data_json(&event) else {
            let mut out = self.flush();
            out.push(event);
            return out;
        };
        if self.pending.is_some() {
            return self.accumulate(json, event);
        }
        let pending_item = function_call_item(&json)
            .filter(|item| is_read_mcp_resource(item) && !arguments_are_complete(item))
            .cloned();
        if let Some(item) = pending_item {
            self.pending = Some(PendingReadMcp {
                arguments: argument_text(&item),
                item,
                events: vec![event],
            });
            return Vec::new();
        }
        if rewrite_tool_call_json(&mut json) {
            return vec![encode_sse_data(&json)];
        }
        vec![event]
    }

    pub fn flush(&mut self) -> Vec<Vec<u8>> {
        let Some(pending) = self.pending.take() else {
            return Vec::new();
        };
        finish_pending(pending)
    }

    fn accumulate(&mut self, json: Value, event: Vec<u8>) -> Vec<Vec<u8>> {
        let kind = json.get("type").and_then(Value::as_str).unwrap_or("");
        let done_arguments = json
            .get("arguments")
            .and_then(Value::as_str)
            .filter(|_| kind.contains("function_call_arguments.done"))
            .map(str::to_string);
        let delta = json
            .get("delta")
            .and_then(Value::as_str)
            .filter(|_| kind.contains("function_call_arguments.delta"))
            .map(str::to_string);
        let item = function_call_item(&json).cloned();
        let item_done = kind.contains("output_item.done");
        let should_flush = {
            let Some(pending) = self.pending.as_mut() else {
                return vec![event];
            };
            if let Some(delta) = delta {
                pending.arguments.push_str(&delta);
                pending.events.push(event);
                false
            } else if let Some(arguments) = done_arguments {
                pending.arguments = arguments.clone();
                pending.events.push(event);
                if let Some(object) = pending.item.as_object_mut() {
                    object.insert("arguments".to_string(), json!(arguments));
                }
                true
            } else if let Some(item) = item {
                pending.item = item;
                if let Some(arguments) = pending.item.get("arguments").and_then(Value::as_str)
                    && !arguments.is_empty()
                {
                    pending.arguments = arguments.to_string();
                }
                let complete = arguments_are_complete(&pending.item) || item_done;
                pending.events.push(event);
                complete
            } else {
                pending.events.push(event);
                false
            }
        };
        if should_flush {
            self.flush()
        } else {
            Vec::new()
        }
    }
}

pub fn request_has_codex_app_tools(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| tools.iter().any(is_codex_app_tool))
}

pub fn rewrite_sse_event(event: &[u8]) -> Vec<u8> {
    let Some(mut json) = sse_data_json(event) else {
        return event.to_vec();
    };
    if !rewrite_tool_call_json(&mut json) {
        return event.to_vec();
    }
    encode_sse_data(&json)
}

fn is_codex_app_tool(tool: &Value) -> bool {
    let name = tool_name(tool);
    let short_name = short_tool_name(name);
    let namespace = tool
        .get("namespace")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let server = tool.get("server").and_then(Value::as_str).unwrap_or("");
    matches!(
        short_name,
        "list_threads" | "read_thread" | "list_archived_threads"
    ) || name.contains("codex_app")
        || name.contains("codex-app")
        || namespace.contains("codex_app")
        || namespace.contains("codex-app")
        || is_codex_app_alias(server)
}

fn rewrite_tool_call_json(value: &mut Value) -> bool {
    let mut changed = false;
    if let Some(item) = function_call_item_mut(value) {
        changed |= rewrite_function_call(item);
    }
    if let Some(output) = value.pointer_mut("/response/output")
        && let Some(items) = output.as_array_mut()
    {
        for item in items {
            changed |= rewrite_function_call(item);
        }
    }
    changed
}

fn rewrite_function_call(item: &mut Value) -> bool {
    let mut changed = false;
    if is_read_mcp_resource(item) {
        changed |= rewrite_complete_read_mcp(item);
    }
    if is_thread_tool(item) {
        changed |= ensure_codex_app_namespace(item);
        changed |= clamp_read_thread_limit(item);
    }
    changed
}

fn rewrite_complete_read_mcp(item: &mut Value) -> bool {
    let Some(args) = parse_arguments(item) else {
        return false;
    };
    let Some(thread_id) = thread_id_from_args(&args) else {
        return false;
    };
    let Some(object) = item.as_object_mut() else {
        return false;
    };
    object.insert("name".to_string(), json!("read_thread"));
    object.insert("namespace".to_string(), json!(CODEX_APP_NAMESPACE));
    object.insert(
        "arguments".to_string(),
        json!(read_thread_arguments(&thread_id, &args)),
    );
    true
}

fn clamp_read_thread_limit(item: &mut Value) -> bool {
    if short_tool_name(tool_name(item)) != "read_thread" {
        return false;
    }
    let Some(mut args) = parse_arguments(item) else {
        return false;
    };
    let Some(object) = args.as_object_mut() else {
        return false;
    };
    let limit = object
        .get("turnLimit")
        .and_then(Value::as_u64)
        .or_else(|| object.get("turn_limit").and_then(Value::as_u64));
    match limit {
        Some(value) if value > READ_THREAD_MAX_TURN_LIMIT => {
            object.insert("turnLimit".to_string(), json!(READ_THREAD_MAX_TURN_LIMIT));
            object.remove("turn_limit");
        }
        _ => return false,
    }
    set_arguments(item, &args)
}

fn ensure_codex_app_namespace(item: &mut Value) -> bool {
    if !is_thread_tool(item) {
        return false;
    }
    match item.get("namespace").and_then(Value::as_str) {
        Some(namespace) if namespace == CODEX_APP_NAMESPACE => false,
        _ => {
            if let Some(object) = item.as_object_mut() {
                object.insert("namespace".to_string(), json!(CODEX_APP_NAMESPACE));
                true
            } else {
                false
            }
        }
    }
}

fn finish_pending(mut pending: PendingReadMcp) -> Vec<Vec<u8>> {
    if !pending.arguments.is_empty()
        && let Some(object) = pending.item.as_object_mut()
    {
        object.insert("arguments".to_string(), json!(pending.arguments.clone()));
    }
    if rewrite_function_call(&mut pending.item) {
        let mut rewritten = json!({
            "type": "response.output_item.added",
            "item": pending.item,
        });
        if let Some(id) = pending.events.iter().find_map(|event| {
            sse_data_json(event)
                .and_then(|json| json.get("id").cloned())
                .filter(|id| !id.is_null())
        }) {
            rewritten
                .as_object_mut()
                .expect("object")
                .insert("id".to_string(), id);
        }
        return vec![encode_sse_data(&rewritten)];
    }
    pending.events
}

fn thread_id_from_args(args: &Value) -> Option<String> {
    if let Some(thread_id) = args
        .get("threadId")
        .or_else(|| args.get("thread_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        return Some(thread_id.to_string());
    }
    let server = args.get("server").and_then(Value::as_str).unwrap_or("");
    let uri = args.get("uri").and_then(Value::as_str).unwrap_or("");
    let thread_id = parse_thread_uri(uri);
    if thread_id.is_some() || is_codex_app_alias(server) {
        return thread_id;
    }
    None
}

fn parse_thread_uri(uri: &str) -> Option<String> {
    let rest = uri
        .strip_prefix("thread://")
        .or_else(|| uri.strip_prefix("codex://thread/"))?;
    rest.split('/')
        .map(str::trim)
        .find(|segment| is_thread_id(segment))
        .map(str::to_string)
}

fn is_thread_id(value: &str) -> bool {
    let mut parts = value.split('-');
    let counts = [8, 4, 4, 4, 12];
    for expected in counts {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.len() != expected || !part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
    }
    parts.next().is_none()
}

fn is_codex_app_alias(server: &str) -> bool {
    let normalized = server.trim().to_ascii_lowercase();
    CODEX_APP_ALIASES.iter().any(|alias| normalized == *alias)
}

fn is_read_mcp_resource(item: &Value) -> bool {
    short_tool_name(tool_name(item)) == "read_mcp_resource"
}

fn is_thread_tool(item: &Value) -> bool {
    matches!(
        short_tool_name(tool_name(item)),
        "list_threads" | "read_thread" | "list_archived_threads"
    )
}

fn tool_name(item: &Value) -> &str {
    item.get("name").and_then(Value::as_str).unwrap_or("")
}

fn short_tool_name(name: &str) -> &str {
    name.rsplit("__")
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(name)
}

fn arguments_are_complete(item: &Value) -> bool {
    parse_arguments(item).is_some()
}

fn argument_text(item: &Value) -> String {
    match item.get("arguments") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(_) | Value::Array(_)) => {
            serde_json::to_string(item.get("arguments").unwrap_or(&Value::Null)).unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn parse_arguments(item: &Value) -> Option<Value> {
    match item.get("arguments") {
        Some(Value::Object(map)) if !map.is_empty() => Some(Value::Object(map.clone())),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str(trimmed).ok()
        }
        _ => None,
    }
}

fn set_arguments(item: &mut Value, args: &Value) -> bool {
    let encoded = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
    let Some(object) = item.as_object_mut() else {
        return false;
    };
    object.insert("arguments".to_string(), json!(encoded));
    true
}

fn read_thread_arguments(thread_id: &str, source: &Value) -> String {
    let include_outputs = source
        .get("includeOutputs")
        .or_else(|| source.get("include_outputs"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let turn_limit = source
        .get("turnLimit")
        .or_else(|| source.get("turn_limit"))
        .and_then(Value::as_u64)
        .unwrap_or(READ_THREAD_MAX_TURN_LIMIT)
        .min(READ_THREAD_MAX_TURN_LIMIT);
    serde_json::to_string(&json!({
        "threadId": thread_id,
        "turnLimit": turn_limit,
        "includeOutputs": include_outputs,
    }))
    .unwrap_or_else(|_| {
        format!(
            "{{\"threadId\":\"{thread_id}\",\"turnLimit\":{READ_THREAD_MAX_TURN_LIMIT},\"includeOutputs\":true}}"
        )
    })
}

fn function_call_item(value: &Value) -> Option<&Value> {
    if is_function_call_node(value) {
        return Some(value);
    }
    let item = value.get("item")?;
    is_function_call_node(item).then_some(item)
}

fn function_call_item_mut(value: &mut Value) -> Option<&mut Value> {
    if is_function_call_node(value) {
        return Some(value);
    }
    let item = value.get_mut("item")?;
    is_function_call_node(item).then_some(item)
}

fn is_function_call_node(value: &Value) -> bool {
    match value.get("type").and_then(Value::as_str) {
        Some("function_call") => true,
        Some(kind) if kind.contains("function_call_arguments") => false,
        Some(kind) if kind.starts_with("response.") => false,
        Some(_) | None => matches!(
            short_tool_name(tool_name(value)),
            "read_mcp_resource" | "read_thread" | "list_threads" | "list_archived_threads"
        ),
    }
}

fn encode_sse_data(value: &Value) -> Vec<u8> {
    let mut event = Vec::from("data: ");
    if let Ok(bytes) = serde_json::to_vec(value) {
        event.extend(bytes);
    }
    event.extend_from_slice(b"\n\n");
    event
}

fn sse_data_json(event: &[u8]) -> Option<Value> {
    let mut data = Vec::new();
    for line in event.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(&[b'\r']).unwrap_or(line);
        let Some(rest) = line.strip_prefix(b"data:") else {
            continue;
        };
        let rest = rest.strip_prefix(b" ").unwrap_or(rest);
        if !data.is_empty() {
            data.push(b'\n');
        }
        data.extend_from_slice(rest);
    }
    if data.is_empty() {
        return None;
    }
    serde_json::from_slice(&data).ok()
}

fn event_is_sse_comment(event: &[u8]) -> bool {
    let event = event.strip_prefix(&[b'\r']).unwrap_or(event);
    event.starts_with(b":")
}

fn event_is_completed(event: &[u8]) -> bool {
    event
        .windows(b"response.completed".len())
        .any(|window| window == b"response.completed")
        || event
            .windows(b"response.incomplete".len())
            .any(|window| window == b"response.incomplete")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse(value: Value) -> Vec<u8> {
        encode_sse_data(&value)
    }

    fn item_from_sse(event: &[u8]) -> Value {
        sse_data_json(event)
            .unwrap()
            .get("item")
            .cloned()
            .unwrap_or_else(|| sse_data_json(event).unwrap())
    }

    #[test]
    fn rewrites_session_alias_payloads_to_read_thread() {
        for server in [
            "codex-app-tools",
            "plugin-codex-app-tools-codex-app",
            "codex-app",
            "codex_app",
        ] {
            let event = sse(json!({
                "type": "response.output_item.added",
                "item": {
                    "type": "function_call",
                    "name": "read_mcp_resource",
                    "call_id": "call-1",
                    "arguments": format!(
                        "{{\"server\":\"{server}\",\"uri\":\"thread://01a0a9ef-acff-7fc2-a141-04e25f2bdac1\"}}"
                    )
                }
            }));
            let rewritten = rewrite_sse_event(&event);
            let item = item_from_sse(&rewritten);
            assert_eq!(item["name"], "read_thread");
            assert_eq!(item["namespace"], CODEX_APP_NAMESPACE);
            let args: Value = serde_json::from_str(item["arguments"].as_str().unwrap()).unwrap();
            assert_eq!(args["threadId"], "01a0a9ef-acff-7fc2-a141-04e25f2bdac1");
            assert_eq!(args["turnLimit"], 10);
            assert_eq!(args["includeOutputs"], true);
            assert!(!String::from_utf8_lossy(&rewritten).contains(server) || server == "codex_app");
        }
    }

    #[test]
    fn clamps_read_thread_turn_limit() {
        let event = sse(json!({
            "type": "response.output_item.added",
            "item": {
                "type": "function_call",
                "name": "read_thread",
                "namespace": "mcp__codex_app",
                "arguments": "{\"threadId\":\"01a0aaca-3310-77c2-aa49-6b7c67abb006\",\"turnLimit\":20,\"includeOutputs\":true}"
            }
        }));
        let item = item_from_sse(&rewrite_sse_event(&event));
        let args: Value = serde_json::from_str(item["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["turnLimit"], 10);
        assert_eq!(args["threadId"], "01a0aaca-3310-77c2-aa49-6b7c67abb006");
    }

    #[test]
    fn leaves_unrelated_mcp_resources_alone() {
        let event = sse(json!({
            "type": "response.output_item.added",
            "item": {
                "type": "function_call",
                "name": "read_mcp_resource",
                "arguments": "{\"server\":\"node_repl\",\"uri\":\"file://workspace/foo.ts\"}"
            }
        }));
        let rewritten = rewrite_sse_event(&event);
        assert_eq!(rewritten, event);
    }

    #[test]
    fn leaves_plugin_file_resources_alone() {
        let event = sse(json!({
            "type": "response.output_item.added",
            "item": {
                "type": "function_call",
                "name": "read_mcp_resource",
                "arguments": "{\"server\":\"codex-app-tools\",\"uri\":\"file:///C:/Users/jians/.codex/plugins/cache/skill.md\"}"
            }
        }));
        assert_eq!(rewrite_sse_event(&event), event);
    }

    #[test]
    fn parses_local_and_turns_thread_uris() {
        assert_eq!(
            parse_thread_uri("thread://local/01a0a9ef-acff-7fc2-a141-04e25f2bdac1/turns")
                .as_deref(),
            Some("01a0a9ef-acff-7fc2-a141-04e25f2bdac1")
        );
        assert_eq!(
            parse_thread_uri("thread://01a0aaca-3310-77c2-aa49-6b7c67abb006").as_deref(),
            Some("01a0aaca-3310-77c2-aa49-6b7c67abb006")
        );
    }

    #[test]
    fn buffers_argument_deltas_then_rewrites() {
        let mut buffer = McpRewriteBuffer::default();
        assert!(
            buffer
                .ingest(sse(json!({
                    "type": "response.output_item.added",
                    "item": {
                        "type": "function_call",
                        "id": "fc_1",
                        "name": "read_mcp_resource",
                        "arguments": ""
                    }
                })))
                .is_empty()
        );
        assert!(
            buffer
                .ingest(sse(json!({
                    "type": "response.function_call_arguments.delta",
                    "delta": "{\"server\":\"codex-app-tools\",\"uri\":\"thread://01a0a9ef-acff-7fc2-a141-04e25f2bdac1\"}"
                })))
                .is_empty()
        );
        let flushed = buffer.ingest(sse(json!({
            "type": "response.function_call_arguments.done",
            "arguments": "{\"server\":\"codex-app-tools\",\"uri\":\"thread://01a0a9ef-acff-7fc2-a141-04e25f2bdac1\"}"
        })));
        assert_eq!(flushed.len(), 1);
        let item = item_from_sse(&flushed[0]);
        assert_eq!(item["name"], "read_thread");
        let args: Value = serde_json::from_str(item["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["threadId"], "01a0a9ef-acff-7fc2-a141-04e25f2bdac1");
        assert!(!String::from_utf8_lossy(&flushed[0]).contains("codex-app-tools"));
    }

    #[test]
    fn detects_codex_app_tools_in_request() {
        assert!(request_has_codex_app_tools(&json!({
            "tools": [
                {"type": "function", "name": "exec_command"},
                {"type": "function", "name": "list_threads", "namespace": "mcp__codex_app"}
            ]
        })));
        assert!(request_has_codex_app_tools(&json!({
            "tools": [{"type": "function", "name": "mcp__codex_app__read_thread"}]
        })));
        assert!(!request_has_codex_app_tools(&json!({
            "tools": [{"type": "function", "name": "exec_command"}]
        })));
        assert!(!request_has_codex_app_tools(&json!({
            "tools": [{"type": "function", "name": "read_mcp_resource"}]
        })));
    }

    #[test]
    fn rewrites_completed_response_output() {
        let mut buffer = McpRewriteBuffer::default();
        let flushed = buffer.ingest(sse(json!({
            "type": "response.completed",
            "response": {
                "id": "resp_1",
                "output": [{
                    "type": "function_call",
                    "name": "read_mcp_resource",
                    "arguments": "{\"server\":\"codex-app\",\"uri\":\"thread://local/01a0a9ef-acff-7fc2-a141-04e25f2bdac1\"}"
                }]
            }
        })));
        assert_eq!(flushed.len(), 1);
        let json = sse_data_json(&flushed[0]).unwrap();
        let item = &json["response"]["output"][0];
        assert_eq!(item["name"], "read_thread");
        assert_eq!(item["namespace"], CODEX_APP_NAMESPACE);
        let args: Value = serde_json::from_str(item["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["threadId"], "01a0a9ef-acff-7fc2-a141-04e25f2bdac1");
        assert_eq!(args["turnLimit"], 10);
        assert!(!String::from_utf8_lossy(&flushed[0]).contains("read_mcp_resource"));
    }
}
