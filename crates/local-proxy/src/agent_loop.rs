//! Keep Codex's tool loop moving when a third-party model answers with a
//! one-line status and then stops.
//!
//! Codex ends a turn as soon as the model emits assistant text without a
//! `function_call`. Grok (and similar models) often do exactly that after
//! Continue. The proxy therefore:
//! - appends a loop instruction whenever tools are present
//! - names the Codex app MCP server when those tools are present
//! - forces `tool_choice=required` on Continue / prior one-liners
//! - holds `response.completed` and retries without a tool call: first the
//!   original Responses body, then a compact Responses body, then a slim
//!   Chat Completions request (`tool_choice=required`, then a named
//!   `exec_command` force). Relays 422 a full 90k-token Chat clone of
//!   Responses, so the Chat body is not the original transcript.
//! - if Chat still cannot produce tool_calls, emits one synthetic
//!   `exec_command` instead of `response.completed` with assistant text.
//!   Codex 0.154 still ends the turn on `response.completed` even when the
//!   leftover status is `phase: "commentary"`.
//! - Chat fallback advertises only exec_command / apply_patch / wait so
//!   Grok cannot loop get_goal / open_in_codex / a fake mcp__codex_app.
//! - collapses repeated status sentences and does not replay them when a
//!   function_call is already in the same response (Codex shows the tool).
//! - rewrites apply_patch `*** Begin Patch ***` to Codex's `*** Begin Patch`.
//! - emits `response.keep_alive` SSE events so Codex's event-level idle
//!   timer stays armed (comment lines are discarded by EventSource)

use serde_json::{Value, json};
use url::Url;

use crate::mcp_compat::{MCP_COMPAT_INSTRUCTION, request_has_codex_app_tools};

pub const SSE_KEEP_ALIVE_HEARTBEAT: &[u8] =
    b"event: response.keep_alive\ndata: {\"type\":\"response.keep_alive\"}\n\n";

pub const AGENT_LOOP_INSTRUCTION: &str = "Codex ends the turn if you output only assistant text. If any inspection, command, or edit remains, you MUST emit a function_call in this same response. A one-line status or promise is not completion.";

pub const AGENT_LOOP_NUDGE: &str = "You ended this turn without a tool call. The previous assistant message is not completion. Immediately call the required tools. Do not output another status sentence.";

pub const AGENT_LOOP_TOOL_STYLE: &str = "Prefer exec_command and apply_patch. Never call get_goal, open_in_codex, list_mcp_resources, or a tool named mcp__codex_app. apply_patch must start with *** Begin Patch with no trailing stars. Do not repeat a status sentence.";

const STATUS_MARKERS: &[&str] = &[
    "不再空转",
    "残渣",
    "对着",
    "直接改",
    "改完",
    "清掉",
    "过一遍",
    "不再",
    "我来",
    "该改",
    "摸清",
    "下手",
    "骨架",
    "先把",
    "let me",
    "i'll",
    "i will",
    "i am going to",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLoopGuard {
    None,
    Instructions,
    ForceTools,
}

impl AgentLoopGuard {
    pub fn as_log_value(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Instructions => Some("instructions"),
            Self::ForceTools => Some("force_tools"),
        }
    }

    pub fn allows_nudge(self) -> bool {
        matches!(self, Self::ForceTools)
    }
}

#[derive(Debug, Default)]
pub struct SseAgentState {
    pub saw_function_call: bool,
    pub output_text: String,
    pub completed_message: String,
    pub response_id: Option<String>,
    pub held_completed: Option<Vec<u8>>,
}

impl SseAgentState {
    pub fn reset_output(&mut self) {
        self.saw_function_call = false;
        self.output_text.clear();
        self.completed_message.clear();
        self.held_completed = None;
    }

    pub fn status_text(&self) -> &str {
        if !self.completed_message.is_empty() {
            &self.completed_message
        } else {
            &self.output_text
        }
    }

    pub fn should_nudge(&self, allow_force: bool) -> bool {
        !self.saw_function_call
            && self.held_completed.is_some()
            && (allow_force || is_status_one_liner(self.status_text()))
    }
}

pub fn apply_agent_loop_guard(body: &mut Value) -> AgentLoopGuard {
    if !request_has_tools(body) {
        return AgentLoopGuard::None;
    }
    append_instructions(body, AGENT_LOOP_INSTRUCTION);
    append_instructions(body, AGENT_LOOP_TOOL_STYLE);
    if request_has_codex_app_tools(body) {
        append_instructions(body, MCP_COMPAT_INSTRUCTION);
    }
    if should_force_tools(body) {
        body.as_object_mut()
            .expect("object shape checked by caller")
            .insert("tool_choice".to_string(), json!("required"));
        AgentLoopGuard::ForceTools
    } else {
        AgentLoopGuard::Instructions
    }
}

pub fn tools_snapshot(body: &Value) -> Option<Value> {
    body.get("tools")
        .filter(|tools| tools.as_array().is_some_and(|items| !items.is_empty()))
        .cloned()
}

pub fn restore_tools_if_missing(body: &mut Value, cached: Option<&Value>) -> bool {
    if request_has_tools(body) {
        return false;
    }
    let Some(tools) =
        cached.filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
    else {
        return false;
    };
    let Some(object) = body.as_object_mut() else {
        return false;
    };
    object.insert("tools".to_string(), tools.clone());
    true
}

/// Retry the original Codex request. Grok HTTP rejects `previous_response_id`
/// of a just-completed `store:false` response, so the nudge must keep the
/// original continuation pointer and input instead of swapping them.
pub fn build_nudge_request(original: &Value, status_text: Option<&str>) -> Value {
    let mut body = prepare_nudge_base(original);
    if let Some(text) = status_text.map(str::trim).filter(|text| !text.is_empty()) {
        append_role_input(&mut body, "assistant", text);
    }
    append_developer_input(&mut body, AGENT_LOOP_NUDGE);
    body
}

/// Smaller follow-up if the full retry still answered with a one-liner.
/// Keep the original input so the model does not re-list the workspace from
/// scratch; drop `previous_response_id` because Grok rejects store:false ids.
pub fn build_compact_nudge_request(original: &Value) -> Value {
    let mut body = prepare_nudge_base(original);
    if let Some(object) = body.as_object_mut() {
        object.remove("previous_response_id");
    }
    append_developer_input(&mut body, AGENT_LOOP_NUDGE);
    body
}

fn prepare_nudge_base(original: &Value) -> Value {
    let mut body = original.clone();
    if let Some(object) = body.as_object_mut() {
        object.insert("tool_choice".to_string(), json!("required"));
        object.insert("stream".to_string(), json!(true));
        object.remove("response_id");
    }
    prepend_instructions(&mut body, AGENT_LOOP_NUDGE);
    body
}

pub fn request_has_tools(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
}

pub fn should_force_tools(body: &Value) -> bool {
    if !request_has_tools(body) || has_tool_result_after_last_user(body) {
        return false;
    }
    should_force_nudge(body)
}

pub fn should_force_nudge(body: &Value) -> bool {
    continue_nudge_in_request(body)
        || last_role_text(body, "assistant")
            .as_deref()
            .is_some_and(is_status_one_liner)
}

pub fn has_tool_result(body: &Value) -> bool {
    input_items(body).iter().any(is_tool_result_item)
}

pub fn has_tool_result_after_last_user(body: &Value) -> bool {
    let items = input_items(body);
    if items.is_empty() {
        return false;
    }
    let start = last_substantive_user_index(&items)
        .map(|index| index + 1)
        .unwrap_or(0);
    items[start..].iter().any(is_tool_result_item)
}

pub fn continue_nudge_in_request(body: &Value) -> bool {
    last_substantive_user_text(body).is_some_and(|text| {
        is_continue_nudge(&text) || is_continue_nudge(&strip_environment_context(&text))
    })
}

pub fn is_continue_nudge(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    matches!(
        trimmed,
        "继续" | "继续完成任务" | "继续任务" | "请继续" | "接着做"
    ) || matches!(
        lower.as_str(),
        "continue" | "continue." | "keep going" | "keep working"
    )
}

pub fn is_status_one_liner(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let collapsed = collapse_repeated_text(trimmed);
    if collapsed != trimmed {
        return true;
    }
    let chars = collapsed.chars().count();
    if chars > 80 || collapsed.lines().count() > 2 {
        return false;
    }
    let lower = collapsed.to_lowercase();
    STATUS_MARKERS
        .iter()
        .any(|marker| lower.contains(marker) || collapsed.contains(marker))
}

/// Grok often concatenates the same status sentence 10–20 times in one
/// output. Collapse that to a single sentence so Codex does not render a
/// wall of repeats.
pub fn collapse_repeated_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let sentences: Vec<&str> = split_status_sentences(trimmed);
    if sentences.len() >= 2 {
        let mut best: Option<(&str, usize)> = None;
        for sentence in &sentences {
            let count = sentences.iter().filter(|item| *item == sentence).count();
            if best.map(|(_, seen)| count > seen).unwrap_or(true) {
                best = Some((*sentence, count));
            }
        }
        if let Some((sentence, count)) = best
            && count >= 2
            && count * 2 >= sentences.len()
        {
            return ensure_sentence_end(sentence);
        }
    }
    if let Some(unit) = repeated_char_unit(trimmed) {
        return unit;
    }
    trimmed.to_string()
}

fn split_status_sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        if matches!(ch, '。' | '！' | '？' | '\n') {
            let end = index + ch.len_utf8();
            let piece = text[start..end].trim();
            if !piece.is_empty() {
                out.push(piece);
            }
            start = end;
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn ensure_sentence_end(sentence: &str) -> String {
    let trimmed = sentence.trim();
    if trimmed.ends_with('。') || trimmed.ends_with('！') || trimmed.ends_with('？') {
        trimmed.to_string()
    } else if trimmed.ends_with('.') || trimmed.ends_with('!') || trimmed.ends_with('?') {
        trimmed.to_string()
    } else {
        format!("{trimmed}。")
    }
}

fn repeated_char_unit(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len < 16 {
        return None;
    }
    let max_unit = (len / 2).min(120);
    for unit_len in 8..=max_unit {
        if len / unit_len < 2 {
            break;
        }
        let unit: String = chars[..unit_len].iter().collect();
        let mut pos = 0;
        let mut copies = 0;
        while pos + unit_len <= len && chars[pos..pos + unit_len] == chars[..unit_len] {
            pos += unit_len;
            copies += 1;
        }
        if copies >= 2 && pos + unit_len > len {
            return Some(unit.trim().to_string());
        }
    }
    None
}

pub fn drain_sse_events(tail: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut events = Vec::new();
    loop {
        let Some(pos) = tail.windows(2).position(|window| window == b"\n\n") else {
            break;
        };
        events.push(tail.drain(..=pos + 1).collect());
    }
    events
}

pub fn is_sse_comment(event: &[u8]) -> bool {
    let event = strip_leading_cr(event);
    event.starts_with(b":")
}

pub fn is_keep_alive_event(event: &[u8]) -> bool {
    contains_seq(event, b"response.keep_alive")
}

pub fn is_completed_event(event: &[u8]) -> bool {
    contains_seq(event, b"response.completed") || contains_seq(event, b"response.incomplete")
}

pub fn is_response_created_event(event: &[u8]) -> bool {
    contains_seq(event, b"response.created")
}

pub fn is_assistant_text_event(event: &[u8]) -> bool {
    if is_sse_comment(event) || is_keep_alive_event(event) || is_completed_event(event) {
        return false;
    }
    if let Some(json) = sse_data_json(event) {
        let kind = json.get("type").and_then(Value::as_str).unwrap_or("");
        if kind.contains("function_call") || kind.contains("reasoning") {
            return false;
        }
        if kind.contains("output_text") || kind.contains("content_part") {
            return true;
        }
        if kind.contains("output_item") {
            let item_type = json
                .pointer("/item/type")
                .and_then(Value::as_str)
                .unwrap_or("");
            return item_type == "message" || item_type == "output_text";
        }
        return false;
    }
    contains_seq(event, b"output_text") && !contains_seq(event, b"function_call")
}

pub fn sse_response_id(event: &[u8]) -> Option<String> {
    sse_data_json(event)?
        .pointer("/response/id")
        .and_then(Value::as_str)
        .filter(|id| id.len() >= 8)
        .map(str::to_string)
}

pub fn note_sse_event(state: &mut SseAgentState, event: &[u8]) {
    if is_sse_comment(event) || is_keep_alive_event(event) {
        return;
    }
    if let Some(json) = sse_data_json(event) {
        if json_has_function_call(&json) {
            state.saw_function_call = true;
        }
        collect_output_text(&json, state);
        if state.response_id.is_none()
            && let Some(id) = json
                .pointer("/response/id")
                .and_then(Value::as_str)
                .filter(|id| id.len() >= 8)
        {
            state.response_id = Some(id.to_string());
        }
    } else if contains_seq(event, b"\"item\":{\"type\":\"function_call\"")
        || contains_seq(event, b"\"item\": {\"type\": \"function_call\"")
        || contains_seq(event, b"\"type\":\"function_call_arguments")
    {
        state.saw_function_call = true;
    }
    if is_completed_event(event) {
        state.held_completed = Some(event.to_vec());
    }
}

pub fn rewrite_response_id(event: &[u8], from: &str, to: &str) -> Vec<u8> {
    if from.is_empty() || from == to {
        return event.to_vec();
    }
    let Ok(text) = std::str::from_utf8(event) else {
        return event.to_vec();
    };
    text.replace(from, to).into_bytes()
}

pub fn commentary_output_events(text: &str) -> Vec<Vec<u8>> {
    let text = collapse_repeated_text(text);
    if text.is_empty() {
        return Vec::new();
    }
    let id = format!("msg_cps_{:x}", fnv1a64(&text));
    let item = json!({
        "id": id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "phase": "commentary",
        "content": [{"type": "output_text", "text": text}]
    });
    let mut added = item.clone();
    if let Some(object) = added.as_object_mut() {
        object.insert("status".to_string(), json!("in_progress"));
        object.insert("content".to_string(), json!([]));
    }
    vec![
        encode_sse_event(&json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": added
        })),
        encode_sse_event(&json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": item
        })),
    ]
}

pub fn rephase_event_as_commentary(event: &[u8]) -> Vec<u8> {
    if is_sse_comment(event) || is_keep_alive_event(event) {
        return event.to_vec();
    }
    let Some(mut json) = sse_data_json(event) else {
        return event.to_vec();
    };
    if !mark_commentary(&mut json) {
        return event.to_vec();
    }
    rebuild_sse_event(event, &json)
}

pub fn chat_completions_url(responses_url: &Url) -> Option<Url> {
    let path = responses_url.path().trim_end_matches('/');
    let stripped = path.strip_suffix("/responses")?;
    let mut url = responses_url.clone();
    let new_path = if stripped.is_empty() {
        "/chat/completions".to_string()
    } else {
        format!("{stripped}/chat/completions")
    };
    url.set_path(&new_path);
    Some(url)
}

const MAX_CHAT_INSTRUCTION_CHARS: usize = 8_000;
const CORE_CHAT_TOOLS: &[&str] = &["exec_command", "apply_patch", "wait"];

struct ChatConvertOpts {
    core_tools_only: bool,
    force_tool: Option<&'static str>,
}

pub fn responses_to_chat_request(body: &Value) -> Value {
    build_chat_request(
        body,
        ChatConvertOpts {
            core_tools_only: true,
            force_tool: None,
        },
    )
}

pub fn responses_to_chat_request_forced(body: &Value) -> Value {
    build_chat_request(
        body,
        ChatConvertOpts {
            core_tools_only: true,
            force_tool: Some("exec_command"),
        },
    )
}

fn build_chat_request(body: &Value, opts: ChatConvertOpts) -> Value {
    let mut messages = Vec::new();
    let mut system = body
        .get("instructions")
        .and_then(Value::as_str)
        .map(|text| truncate_chars(text, MAX_CHAT_INSTRUCTION_CHARS))
        .unwrap_or_default();
    if !system.contains(AGENT_LOOP_NUDGE) {
        if !system.is_empty() && !system.ends_with('\n') {
            system.push('\n');
        }
        system.push_str(AGENT_LOOP_NUDGE);
    }
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    let user = last_substantive_user_text(body).unwrap_or_else(|| "继续完成任务".to_string());
    messages.push(json!({"role": "user", "content": user}));
    if let Some(assistant) = last_role_text(body, "assistant").filter(|text| !text.is_empty()) {
        messages.push(json!({"role": "assistant", "content": assistant}));
    }
    messages.push(json!({"role": "user", "content": AGENT_LOOP_NUDGE}));

    let tool_choice = match opts.force_tool {
        Some(name) => json!({"type": "function", "function": {"name": name}}),
        None => json!("required"),
    };
    let mut chat = json!({
        "model": body.get("model").cloned().unwrap_or(json!("")),
        "messages": messages,
        "stream": false,
        "tool_choice": tool_choice,
    });
    let converted = chat_tools(body, opts.core_tools_only);
    if !converted.is_empty() {
        chat.as_object_mut()
            .expect("object literal")
            .insert("tools".to_string(), json!(converted));
    }
    chat
}

fn chat_tools(body: &Value, core_only: bool) -> Vec<Value> {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    tools
        .iter()
        .filter_map(|tool| {
            let converted = responses_tool_to_chat(tool);
            let name = converted
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if name.is_empty() {
                return None;
            }
            if core_only && !CORE_CHAT_TOOLS.contains(&name) {
                return None;
            }
            Some(converted)
        })
        .collect()
}

pub fn synthetic_tool_call_sse(body: &Value, response_id: &str) -> Option<Vec<u8>> {
    if has_tool_result_after_last_user(body) {
        return None;
    }
    let has_exec = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool_name(tool) == Some("exec_command"))
        });
    if !has_exec {
        return None;
    }
    let cwd = cwd_from_body(body).unwrap_or_else(|| ".".to_string());
    let args = serde_json::to_string(&json!({
        "cmd": "Get-ChildItem -File | Select-Object -First 40 Name, Length, LastWriteTime",
        "workdir": cwd
    }))
    .ok()?;
    chat_completion_to_responses_sse(
        &json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_cps_synthetic",
                        "type": "function",
                        "function": {
                            "name": "exec_command",
                            "arguments": args
                        }
                    }]
                }
            }]
        }),
        response_id,
    )
}

fn tool_name(tool: &Value) -> Option<&str> {
    tool.pointer("/function/name")
        .and_then(Value::as_str)
        .or_else(|| tool.get("name").and_then(Value::as_str))
        .filter(|name| !name.is_empty())
}

fn cwd_from_body(body: &Value) -> Option<String> {
    for item in input_items(body) {
        let text = item_text(&item);
        let Some(start) = text.find("<cwd>") else {
            continue;
        };
        let rest = &text[start + 5..];
        let Some(end) = rest.find("</cwd>") else {
            continue;
        };
        let cwd = rest[..end].trim();
        if !cwd.is_empty() {
            return Some(cwd.to_string());
        }
    }
    None
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

pub fn parse_chat_completion_body(bytes: &[u8], content_type: &str) -> Option<Value> {
    if content_type.contains("event-stream")
        || bytes.starts_with(b"data:")
        || bytes.windows(6).any(|window| window == b"data: ")
    {
        return accumulate_chat_sse(bytes);
    }
    serde_json::from_slice(bytes).ok()
}

pub fn chat_completion_to_responses_sse(chat: &Value, response_id: &str) -> Option<Vec<u8>> {
    let message = chat.pointer("/choices/0/message")?;
    let tool_calls = message.get("tool_calls").and_then(Value::as_array)?;
    if tool_calls.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    let mut output_items = Vec::new();
    for (index, call) in tool_calls.iter().enumerate() {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .unwrap_or("call_chat");
        let name = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .unwrap_or("exec_command");
        let mut args = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}")
            .to_string();
        if name == "apply_patch" {
            args = normalize_apply_patch_args(&args);
        }
        let item = json!({
            "type": "function_call",
            "id": format!("fc_{id}"),
            "call_id": id,
            "name": name,
            "arguments": args,
        });
        out.extend(encode_sse_event(&json!({
            "type": "response.output_item.added",
            "output_index": index,
            "item": item,
        })));
        out.extend(encode_sse_event(&json!({
            "type": "response.output_item.done",
            "output_index": index,
            "item": item,
        })));
        output_items.push(item);
    }
    out.extend(encode_sse_event(&json!({
        "type": "response.completed",
        "response": {
            "id": response_id,
            "output": output_items,
        }
    })));
    Some(out)
}

/// Codex's apply_patch verifier requires the first line to be exactly
/// `*** Begin Patch`. Grok often emits `*** Begin Patch ***`.
pub fn rewrite_apply_patch_event(event: &[u8]) -> Vec<u8> {
    if is_sse_comment(event) || is_keep_alive_event(event) {
        return event.to_vec();
    }
    let Some(mut json) = sse_data_json(event) else {
        return event.to_vec();
    };
    let mut changed = false;
    if let Some(item) = json.get_mut("item") {
        changed |= rewrite_apply_patch_item(item);
    }
    if let Some(output) = json
        .pointer_mut("/response/output")
        .and_then(Value::as_array_mut)
    {
        for item in output {
            changed |= rewrite_apply_patch_item(item);
        }
    }
    if !changed {
        return event.to_vec();
    }
    rebuild_sse_event(event, &json)
}

fn rewrite_apply_patch_item(item: &mut Value) -> bool {
    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
    if name != "apply_patch" {
        return false;
    }
    let mut changed = false;
    if let Some(input) = item
        .get("input")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        let rewritten = normalize_apply_patch_text(&input);
        if rewritten != input
            && let Some(object) = item.as_object_mut()
        {
            object.insert("input".to_string(), json!(rewritten));
            changed = true;
        }
    }
    if let Some(args) = item
        .get("arguments")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        let rewritten = normalize_apply_patch_args(&args);
        if rewritten != args
            && let Some(object) = item.as_object_mut()
        {
            object.insert("arguments".to_string(), json!(rewritten));
            changed = true;
        }
    }
    changed
}

pub fn normalize_apply_patch_text(text: &str) -> String {
    let text = text.trim_start_matches('\u{feff}').trim();
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "*** End of File ***" || trimmed == "*** End of File" {
            continue;
        }
        if trimmed == "*** Begin Patch ***" || trimmed == "*** Begin Patch" {
            lines.push("*** Begin Patch");
            continue;
        }
        if trimmed == "*** End Patch ***" || trimmed == "*** End Patch" {
            lines.push("*** End Patch");
            continue;
        }
        lines.push(line.trim_end());
    }
    let mut out = lines.join("\n");
    if let Some(index) = out.find("*** Begin Patch") {
        out = out[index..].to_string();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn normalize_apply_patch_args(args: &str) -> String {
    match serde_json::from_str::<Value>(args) {
        Ok(Value::Object(mut map)) => {
            if let Some(Value::String(input)) = map.get("input").cloned() {
                map.insert(
                    "input".to_string(),
                    json!(normalize_apply_patch_text(&input)),
                );
            }
            serde_json::to_string(&Value::Object(map)).unwrap_or_else(|_| args.to_string())
        }
        Ok(Value::String(text)) => {
            serde_json::to_string(&normalize_apply_patch_text(&text)).unwrap_or(text)
        }
        _ => {
            if args.contains("*** Begin Patch") {
                normalize_apply_patch_text(args)
            } else {
                args.to_string()
            }
        }
    }
}

fn fnv1a64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn encode_sse_event(value: &Value) -> Vec<u8> {
    let mut event = b"data: ".to_vec();
    event.extend(serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()));
    event.extend_from_slice(b"\n\n");
    event
}

fn mark_commentary(value: &mut Value) -> bool {
    let mut changed = false;
    if let Some(item) = value.get_mut("item") {
        changed |= mark_item_commentary(item);
    }
    if let Some(output) = value
        .pointer_mut("/response/output")
        .and_then(Value::as_array_mut)
    {
        for item in output {
            changed |= mark_item_commentary(item);
        }
    }
    changed
}

fn mark_item_commentary(item: &mut Value) -> bool {
    let is_message = item.get("type").and_then(Value::as_str) == Some("message")
        || item.get("role").and_then(Value::as_str) == Some("assistant");
    if !is_message {
        return false;
    }
    item.as_object_mut()
        .map(|object| {
            object.insert("phase".to_string(), json!("commentary"));
            true
        })
        .unwrap_or(false)
}

fn rebuild_sse_event(original: &[u8], data: &Value) -> Vec<u8> {
    let Ok(json) = serde_json::to_vec(data) else {
        return original.to_vec();
    };
    let mut out = Vec::new();
    for line in original.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(&[b'\r']).unwrap_or(line);
        if line.starts_with(b"event:") {
            out.extend_from_slice(line);
            out.extend_from_slice(b"\n");
        }
    }
    out.extend_from_slice(b"data: ");
    out.extend(json);
    out.extend_from_slice(b"\n\n");
    out
}

#[allow(dead_code)]
fn input_item_to_chat_message(item: &Value) -> Option<Value> {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "function_call" || kind == "custom_tool_call" {
        let id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)?;
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("exec_command");
        let args = item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        return Some(json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [{
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": args}
            }]
        }));
    }
    if is_tool_result_item(item) {
        let id = item.get("call_id").and_then(Value::as_str)?;
        let content = match item.get("output") {
            Some(Value::String(text)) => text.clone(),
            Some(other) => text_from_content(other),
            None => String::new(),
        };
        return Some(json!({
            "role": "tool",
            "tool_call_id": id,
            "content": content
        }));
    }
    let role = match item.get("role").and_then(Value::as_str).unwrap_or("user") {
        "assistant" => "assistant",
        "system" | "developer" => "system",
        _ => "user",
    };
    let text = item_text(item);
    if text.is_empty() {
        return None;
    }
    Some(json!({"role": role, "content": text}))
}

fn responses_tool_to_chat(tool: &Value) -> Value {
    if tool.get("function").is_some() {
        return json!({
            "type": "function",
            "function": tool.get("function").cloned().unwrap_or(json!({}))
        });
    }
    json!({
        "type": "function",
        "function": {
            "name": tool.get("name").cloned().unwrap_or(json!("")),
            "description": tool.get("description").cloned().unwrap_or(json!("")),
            "parameters": tool.get("parameters").cloned().unwrap_or(json!({
                "type": "object",
                "properties": {}
            })),
        }
    })
}

fn accumulate_chat_sse(bytes: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut content = String::new();
    let mut saw_completion = false;
    for block in text.split("\n\n") {
        let Some(data) = block.lines().find_map(|line| line.strip_prefix("data:")) else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(json) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(message) = json.pointer("/choices/0/message") {
            return Some(json!({
                "choices": [{ "message": message.clone() }]
            }));
        }
        let Some(delta) = json.pointer("/choices/0/delta") else {
            continue;
        };
        saw_completion = true;
        if let Some(piece) = delta.get("content").and_then(Value::as_str) {
            content.push_str(piece);
        }
        let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            while tool_calls.len() <= index {
                tool_calls.push(json!({
                    "id": "",
                    "type": "function",
                    "function": {"name": "", "arguments": ""}
                }));
            }
            merge_chat_tool_call(&mut tool_calls[index], call);
        }
    }
    if !saw_completion {
        return None;
    }
    let message = json!({
        "role": "assistant",
        "content": if content.is_empty() { Value::Null } else { json!(content) },
        "tool_calls": tool_calls,
    });
    Some(json!({ "choices": [{ "message": message }] }))
}

fn merge_chat_tool_call(target: &mut Value, delta: &Value) {
    let Some(object) = target.as_object_mut() else {
        return;
    };
    if let Some(id) = delta
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        object.insert("id".to_string(), json!(id));
    }
    let Some(function) = object
        .entry("function".to_string())
        .or_insert_with(|| json!({"name": "", "arguments": ""}))
        .as_object_mut()
    else {
        return;
    };
    if let Some(name) = delta
        .pointer("/function/name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        function.insert("name".to_string(), json!(name));
    }
    if let Some(args) = delta.pointer("/function/arguments").and_then(Value::as_str) {
        let mut merged = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        merged.push_str(args);
        function.insert("arguments".to_string(), json!(merged));
    }
}

fn append_instructions(body: &mut Value, extra: &str) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    match object.get("instructions") {
        Some(Value::String(existing)) if !existing.contains(extra) => {
            let mut merged = String::with_capacity(existing.len() + extra.len() + 2);
            merged.push_str(existing);
            if !existing.ends_with('\n') {
                merged.push('\n');
            }
            merged.push_str(extra);
            object.insert("instructions".to_string(), json!(merged));
        }
        Some(Value::String(_)) => {}
        _ => {
            object.insert("instructions".to_string(), json!(extra));
        }
    }
}

fn prepend_instructions(body: &mut Value, extra: &str) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    let merged = match object.get("instructions") {
        Some(Value::String(existing)) if existing.contains(extra) => return,
        Some(Value::String(existing)) => format!("{extra}\n{existing}"),
        _ => extra.to_string(),
    };
    object.insert("instructions".to_string(), json!(merged));
}

fn append_developer_input(body: &mut Value, text: &str) {
    append_role_input(body, "developer", text);
}

fn append_role_input(body: &mut Value, role: &str, text: &str) {
    let content_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let item = json!({
        "role": role,
        "content": [{"type": content_type, "text": text}]
    });
    let Some(object) = body.as_object_mut() else {
        return;
    };
    match object.get_mut("input") {
        Some(Value::Array(items)) => items.push(item),
        Some(Value::String(existing)) => {
            let wrapped = json!([
                {"role": "user", "content": [{"type": "input_text", "text": existing}]},
                item
            ]);
            object.insert("input".to_string(), wrapped);
        }
        _ => {
            object.insert("input".to_string(), json!([item]));
        }
    }
}

fn last_role_text(body: &Value, role: &str) -> Option<String> {
    let input = body.get("input")?;
    match input {
        Value::String(text) if role == "user" => Some(text.clone()),
        Value::Array(items) => items.iter().rev().find_map(|item| {
            let item_role = item.get("role").and_then(Value::as_str)?;
            (item_role == role).then(|| text_from_content(item.get("content").unwrap_or(item)))
        }),
        _ => None,
    }
}

fn input_items(body: &Value) -> Vec<Value> {
    match body.get("input") {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(text)) => vec![json!({
            "role": "user",
            "content": [{"type": "input_text", "text": text}]
        })],
        _ => Vec::new(),
    }
}

fn is_tool_result_item(item: &Value) -> bool {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    matches!(
        kind,
        "function_call_output" | "tool_result" | "custom_tool_call_output"
    ) || (item.get("call_id").is_some() && item.get("output").is_some())
}

fn item_text(item: &Value) -> String {
    text_from_content(item.get("content").unwrap_or(item))
}

fn is_skippable_user_text(text: &str) -> bool {
    let stripped = strip_environment_context(text);
    stripped.is_empty()
        || stripped.contains("<in-app-browser-context")
        || stripped.contains("ambient-ui-state")
}

fn last_substantive_user_index(items: &[Value]) -> Option<usize> {
    items.iter().enumerate().rev().find_map(|(index, item)| {
        if !is_user_item(item) {
            return None;
        }
        let text = item_text(item);
        (!is_skippable_user_text(&text)).then_some(index)
    })
}

fn last_substantive_user_text(body: &Value) -> Option<String> {
    let items = input_items(body);
    let index = last_substantive_user_index(&items)?;
    Some(item_text(&items[index]))
}

fn is_user_item(item: &Value) -> bool {
    if item.get("role").and_then(Value::as_str) == Some("user") {
        return true;
    }
    matches!(item.get("type").and_then(Value::as_str), Some("input_text"))
}

fn strip_environment_context(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<environment_context>") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start..];
        match after_start.find("</environment_context>") {
            Some(end_rel) => {
                rest = &after_start[end_rel + "</environment_context>".len()..];
            }
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn text_from_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.as_str())
            })
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
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

fn json_has_function_call(value: &Value) -> bool {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "function_call"
        || kind.contains("function_call")
        || kind.contains("custom_tool_call")
    {
        return true;
    }
    if value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|item_type| item_type == "function_call" || item_type == "custom_tool_call")
    {
        return true;
    }
    value
        .pointer("/response/output")
        .and_then(Value::as_array)
        .is_some_and(|output| {
            output.iter().any(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("function_call" | "custom_tool_call")
                )
            })
        })
}

fn collect_output_text(value: &Value, state: &mut SseAgentState) {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    if kind.contains("output_text.delta") || kind == "response.output_text.delta" {
        if let Some(delta) = value.get("delta").and_then(Value::as_str) {
            state.output_text.push_str(delta);
        }
    }
    if let Some(message) = value.get("item").and_then(assistant_message_text) {
        state.completed_message = message;
    }
    if let Some(output) = value.pointer("/response/output").and_then(Value::as_array)
        && let Some(message) = output.iter().rev().find_map(assistant_message_text)
    {
        state.completed_message = message;
    }
}

fn assistant_message_text(item: &Value) -> Option<String> {
    let is_assistant_message = item.get("type").and_then(Value::as_str) == Some("message")
        && item.get("role").and_then(Value::as_str) != Some("user");
    if !is_assistant_message {
        return None;
    }
    let text = text_from_content(item.get("content").unwrap_or(item));
    (!text.is_empty()).then_some(text)
}

fn strip_leading_cr(event: &[u8]) -> &[u8] {
    event.strip_prefix(&[b'\r']).unwrap_or(event)
}

fn contains_seq(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_after_one_liner_forces_tool_choice() {
        let mut body = json!({
            "model": "grok-4.6",
            "tools": [{"type": "function", "name": "exec_command"}],
            "instructions": "Be a coding agent.",
            "input": [
                {
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "对着现成截图改，不再空转。"}]
                },
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": "继续"}]
                }
            ]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::ForceTools
        );
        assert_eq!(body["tool_choice"], "required");
        assert!(
            body["instructions"]
                .as_str()
                .unwrap()
                .contains("function_call")
        );
        assert_ne!(
            body["input"].as_array().unwrap().last().unwrap()["role"],
            "developer"
        );
    }

    #[test]
    fn continue_then_environment_context_still_forces_tools() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "previous_response_id": "resp_prev12345",
            "input": [
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": "继续"}]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "<environment_context>\n  <current_date>2026-09-17</current_date>\n</environment_context>"
                    }]
                }
            ]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::ForceTools
        );
        assert_eq!(body["tool_choice"], "required");
    }

    #[test]
    fn function_call_output_does_not_force_tools() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "previous_response_id": "resp_prev12345",
            "input": [
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": "继续"}]
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "TCP 0.0.0.0:3000 LISTENING"
                }
            ]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::Instructions
        );
        assert!(body.get("tool_choice").is_none());
        assert!(continue_nudge_in_request(&body));
        assert!(has_tool_result(&body));
        assert!(has_tool_result_after_last_user(&body));
    }

    #[test]
    fn historical_continue_with_tool_output_does_not_force() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": "继续完成任务"}]
                },
                {
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "先看截图和当前前端，再直接改。"}]
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "Mode LastWriteTime Name"
                }
            ]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::Instructions
        );
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn new_continue_after_tool_history_still_forces() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "Mode LastWriteTime Name"
                },
                {
                    "role": "user",
                    "content": [{"type": "input_text", "text": "继续"}]
                }
            ]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::ForceTools
        );
        assert_eq!(body["tool_choice"], "required");
    }

    #[test]
    fn continue_inside_environment_context_message_forces_tools() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<environment_context>\n  <current_date>2026-09-17</current_date>\n</environment_context>\n\n继续"
                }]
            }]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::ForceTools
        );
    }

    #[test]
    fn greeting_without_continue_does_not_force_tools() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "在吗"}]
            }]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::Instructions
        );
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn requests_without_tools_are_left_alone() {
        let mut body = json!({"model": "grok-4.6", "input": "hello"});
        assert_eq!(apply_agent_loop_guard(&mut body), AgentLoopGuard::None);
        assert!(body.get("instructions").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn status_one_liner_matches_collapse_phrase() {
        assert!(is_status_one_liner("对着现成截图改，不再空转。"));
        assert!(is_status_one_liner("别催了，残渣直接改完。"));
        assert!(is_status_one_liner(
            "接下来把全局样式和故事页结构摸清，确认头图、日历和搜索提示该改哪。接下来把全局样式和故事页结构摸清，确认头图、日历和搜索提示该改哪。接下来把全局样式和故事页结构摸清，确认头图、日历和搜索提示该改哪。"
        ));
        assert!(!is_status_one_liner("海鸥在线，你要整点薯条吗？"));
        assert!(!is_status_one_liner(
            "装完了。D 盘只剩 4.56GB，塞不下，已经落到 E 盘。Word / Excel 都能从开始菜单打开。"
        ));
    }

    #[test]
    fn collapse_repeated_status_keeps_one_sentence() {
        let repeated =
            "接下来把全局样式和故事页结构摸清，确认头图、日历和搜索提示该改哪。".repeat(18);
        let collapsed = collapse_repeated_text(&repeated);
        assert_eq!(
            collapsed,
            "接下来把全局样式和故事页结构摸清，确认头图、日历和搜索提示该改哪。"
        );
        assert_eq!(collapsed.matches("接下来把全局").count(), 1);
    }

    #[test]
    fn continue_nudge_matches_codex_button_text() {
        assert!(is_continue_nudge("继续"));
        assert!(is_continue_nudge("继续\n"));
        assert!(is_continue_nudge("继续完成任务"));
        assert!(is_continue_nudge("Continue"));
        assert!(!is_continue_nudge("继续苹果风并删第二信源"));
    }

    #[test]
    fn sse_parser_holds_completed_and_detects_missing_tools() {
        let mut tail = format!(
            "data: {{\"type\":\"response.created\",\"response\":{{\"id\":\"resp_abc12345\"}}}}\n\n\
             data: {{\"type\":\"response.output_text.delta\",\"delta\":\"对着现成截图改，不再空转。\"}}\n\n\
             data: {{\"type\":\"response.completed\"}}\n\npartial"
        )
        .into_bytes();
        let events = drain_sse_events(&mut tail);
        assert_eq!(events.len(), 3);
        assert_eq!(tail, b"partial");
        let mut state = SseAgentState::default();
        for event in &events {
            note_sse_event(&mut state, event);
        }
        assert_eq!(state.response_id.as_deref(), Some("resp_abc12345"));
        assert!(!state.saw_function_call);
        assert!(state.should_nudge(false));
        assert!(state.held_completed.is_some());
    }

    #[test]
    fn sse_parser_reads_one_liner_from_output_item_done() {
        let mut state = SseAgentState::default();
        note_sse_event(
            &mut state,
            format!(
                "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"对着现成截图改，不再空转。\"}}]}}}}\n\n"
            )
            .as_bytes(),
        );
        note_sse_event(&mut state, b"data: {\"type\":\"response.completed\"}\n\n");
        assert!(!state.saw_function_call);
        assert_eq!(state.status_text(), "对着现成截图改，不再空转。");
        assert!(state.should_nudge(false));
    }

    #[test]
    fn sse_parser_does_not_treat_delta_plus_done_as_too_long() {
        let mut state = SseAgentState::default();
        note_sse_event(
            &mut state,
            format!(
                "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"对着现成截图改，不再空转。\"}}\n\n"
            )
            .as_bytes(),
        );
        note_sse_event(
            &mut state,
            format!(
                "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"对着现成截图改，不再空转。\"}}]}}}}\n\n"
            )
            .as_bytes(),
        );
        note_sse_event(&mut state, b"data: {\"type\":\"response.completed\"}\n\n");
        assert!(state.should_nudge(false));
        assert_eq!(state.status_text().chars().count(), 13);
    }

    #[test]
    fn sse_parser_does_not_nudge_after_function_call() {
        let mut state = SseAgentState::default();
        note_sse_event(
            &mut state,
            b"data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\"}}\n\n",
        );
        note_sse_event(&mut state, b"data: {\"type\":\"response.completed\"}\n\n");
        assert!(state.saw_function_call);
        assert!(!state.should_nudge(true));
    }

    #[test]
    fn sse_response_id_reads_created_event() {
        assert_eq!(
            sse_response_id(
                b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_abc12345\"}}\n\n"
            )
            .as_deref(),
            Some("resp_abc12345")
        );
    }

    #[test]
    fn nudge_request_keeps_original_previous_response_id() {
        let original = json!({
            "model": "grok-4.6",
            "previous_response_id": "resp_prev12345",
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "继续完成任务"}]}]
        });
        let nudged = build_nudge_request(&original, Some("对着截图把残渣清掉。"));
        assert_eq!(nudged["previous_response_id"], "resp_prev12345");
        assert_eq!(nudged["tool_choice"], "required");
        assert_eq!(nudged["model"], "grok-4.6");
        let input = nudged["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"][0]["text"], "对着截图把残渣清掉。");
        assert_eq!(input.last().unwrap()["role"], "developer");
        assert!(
            nudged["instructions"]
                .as_str()
                .unwrap()
                .starts_with(AGENT_LOOP_NUDGE)
        );
    }

    #[test]
    fn compact_nudge_keeps_history_and_drops_previous_response_id() {
        let original = json!({
            "model": "grok-4.6",
            "previous_response_id": "resp_prev12345",
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "继续完成任务"}]},
                {"role": "user", "content": [{"type": "input_text", "text": "<environment_context>\n  <cwd>D:/proj</cwd>\n</environment_context>"}]}
            ]
        });
        let nudged = build_compact_nudge_request(&original);
        assert!(nudged.get("previous_response_id").is_none());
        assert_eq!(nudged["tool_choice"], "required");
        let input = nudged["input"].as_array().unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["content"][0]["text"], "继续完成任务");
        assert_eq!(input[1]["role"], "user");
        assert_eq!(input[2]["role"], "developer");
    }

    #[test]
    fn restore_tools_fills_empty_continue() {
        let cached = json!([{"type": "function", "name": "exec_command"}]);
        let mut body = json!({
            "model": "grok-4.6",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "继续"}]}]
        });
        assert!(restore_tools_if_missing(&mut body, Some(&cached)));
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::ForceTools
        );
        assert_eq!(body["tool_choice"], "required");
    }

    #[test]
    fn codex_app_tools_get_mcp_server_instruction() {
        let mut body = json!({
            "tools": [
                {"type": "function", "name": "exec_command"},
                {"type": "function", "name": "list_threads", "namespace": "mcp__codex_app"}
            ],
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "继续苹果风"}]}]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::Instructions
        );
        let instructions = body["instructions"].as_str().unwrap();
        assert!(instructions.contains("function_call"));
        assert!(instructions.contains("codex_app"));
        assert!(instructions.contains("read_mcp_resource"));
    }

    #[test]
    fn exec_command_only_does_not_get_mcp_server_instruction() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "在吗"}]}]
        });
        apply_agent_loop_guard(&mut body);
        let instructions = body["instructions"].as_str().unwrap();
        assert!(instructions.contains("function_call"));
        assert!(!instructions.contains("read_mcp_resource"));
        assert!(instructions.contains("apply_patch"));
    }

    #[test]
    fn commentary_events_mark_phase() {
        let events = commentary_output_events(
            "对着现成截图改，不再空转。对着现成截图改，不再空转。对着现成截图改，不再空转。",
        );
        assert_eq!(events.len(), 2);
        let joined = events
            .iter()
            .flat_map(|event| event.iter().copied())
            .collect::<Vec<_>>();
        let text = String::from_utf8(joined).unwrap();
        assert!(text.contains("\"phase\":\"commentary\""));
        assert_eq!(text.matches("对着现成截图改，不再空转。").count(), 1);
        assert!(!text.contains("\"id\":\"msg_cps_status\""));
    }

    #[test]
    fn apply_patch_header_stars_are_stripped() {
        let raw = "*** Begin Patch ***\n*** Update File: src/app.css\n@@\n-a\n+b\n*** End Patch ***\n*** End of File ***\n";
        let normalized = normalize_apply_patch_text(raw);
        assert!(normalized.starts_with("*** Begin Patch\n"));
        assert!(!normalized.contains("*** Begin Patch ***"));
        assert!(normalized.contains("*** End Patch\n"));
        assert!(!normalized.contains("*** End of File"));
    }

    #[test]
    fn rewrite_apply_patch_sse_fixes_custom_tool_input() {
        let event = format!(
            "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"custom_tool_call\",\"name\":\"apply_patch\",\"input\":\"*** Begin Patch ***\\n*** Update File: a.rs\\n@@\\n-a\\n+b\\n*** End Patch ***\\n\"}}}}\n\n"
        );
        let rewritten = rewrite_apply_patch_event(event.as_bytes());
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.contains("*** Begin Patch\\n"));
        assert!(!text.contains("*** Begin Patch ***"));
    }

    #[test]
    fn custom_tool_call_counts_as_function_call() {
        let mut state = SseAgentState::default();
        note_sse_event(
            &mut state,
            b"data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"custom_tool_call\",\"name\":\"apply_patch\"}}\n\n",
        );
        note_sse_event(&mut state, b"data: {\"type\":\"response.completed\"}\n\n");
        assert!(state.saw_function_call);
        assert!(!state.should_nudge(true));
    }

    #[test]
    fn rephase_adds_commentary_to_message_item() {
        let event = format!(
            "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"对着现成截图改，不再空转。\"}}]}}}}\n\n"
        );
        let rewritten = rephase_event_as_commentary(event.as_bytes());
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.contains("\"phase\":\"commentary\""));
    }

    #[test]
    fn chat_url_is_sibling_of_responses() {
        let responses = Url::parse("http://127.0.0.1:18080/v1/responses").unwrap();
        assert_eq!(
            chat_completions_url(&responses).unwrap().as_str(),
            "http://127.0.0.1:18080/v1/chat/completions"
        );
    }

    #[test]
    fn responses_to_chat_request_maps_tools_and_required() {
        let body = json!({
            "model": "grok-4.6",
            "instructions": "Be a coding agent.",
            "previous_response_id": "resp_prev12345",
            "tools": [
                {"type": "function", "name": "exec_command", "parameters": {"type": "object"}},
                {"type": "function", "name": "list_threads", "parameters": {"type": "object"}}
            ],
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "继续"}]},
                {"role": "assistant", "content": [{"type": "output_text", "text": "对着现成截图改，不再空转。"}]}
            ]
        });
        let chat = responses_to_chat_request(&body);
        assert_eq!(chat["model"], "grok-4.6");
        assert_eq!(chat["stream"], false);
        assert_eq!(chat["tool_choice"], "required");
        assert!(chat.get("previous_response_id").is_none());
        assert_eq!(chat["tools"].as_array().unwrap().len(), 1);
        assert_eq!(chat["tools"][0]["function"]["name"], "exec_command");
        let messages = chat["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "继续");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"], AGENT_LOOP_NUDGE);
    }

    #[test]
    fn responses_to_chat_request_forced_names_exec_command() {
        let body = json!({
            "model": "grok-4.6",
            "tools": [
                {"type": "function", "name": "exec_command", "parameters": {"type": "object"}},
                {"type": "function", "name": "list_threads", "parameters": {"type": "object"}},
                {"type": "function", "name": "apply_patch", "parameters": {"type": "object"}}
            ],
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "继续完成任务"}]}]
        });
        let chat = responses_to_chat_request_forced(&body);
        assert_eq!(chat["tool_choice"]["type"], "function");
        assert_eq!(chat["tool_choice"]["function"]["name"], "exec_command");
        let names: Vec<&str> = chat["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["exec_command", "apply_patch"]);
    }

    #[test]
    fn synthetic_tool_call_sse_emits_exec_command() {
        let body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "继续完成任务"}]},
                {"role": "user", "content": [{"type": "input_text", "text": "<environment_context>\n  <cwd>D:/proj</cwd>\n</environment_context>"}]}
            ]
        });
        let sse = synthetic_tool_call_sse(&body, "resp_orig12345").unwrap();
        let text = String::from_utf8(sse).unwrap();
        assert!(text.contains("function_call"));
        assert!(text.contains("exec_command"));
        assert!(text.contains("call_cps_synthetic"));
        assert!(text.contains("Get-ChildItem"));
        assert!(text.contains("D:/proj"));
        assert!(text.contains("resp_orig12345"));
        assert!(text.contains("response.completed"));
    }

    #[test]
    fn synthetic_tool_call_sse_skips_after_tool_result() {
        let body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": "继续完成任务"}]},
                {"type": "function_call_output", "call_id": "call_1", "output": "ok"}
            ]
        });
        assert!(synthetic_tool_call_sse(&body, "resp_orig12345").is_none());
    }

    #[test]
    fn chat_completion_with_tool_calls_becomes_function_call_sse() {
        let chat = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "exec_command",
                            "arguments": "{\"cmd\":\"Get-ChildItem\"}"
                        }
                    }]
                }
            }]
        });
        let sse = chat_completion_to_responses_sse(&chat, "resp_orig12345").unwrap();
        let text = String::from_utf8(sse).unwrap();
        assert!(text.contains("function_call"));
        assert!(text.contains("exec_command"));
        assert!(text.contains("call_abc"));
        assert!(text.contains("resp_orig12345"));
        assert!(text.contains("response.completed"));
        assert!(!text.contains("response.created"));
    }

    #[test]
    fn chat_completion_without_tool_calls_is_none() {
        let chat = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "对着现成截图改，不再空转。"
                }
            }]
        });
        assert!(chat_completion_to_responses_sse(&chat, "resp_orig12345").is_none());
    }

    #[test]
    fn parse_chat_sse_accumulates_tool_calls() {
        let bytes = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"exec_command\",\"arguments\":\"{\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let chat = parse_chat_completion_body(bytes.as_bytes(), "text/event-stream").unwrap();
        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_1"
        );
        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "exec_command"
        );
        assert_eq!(
            chat["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
    }
}
