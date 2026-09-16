//! Keep Codex's tool loop moving when a third-party model answers with a
//! one-line status and then stops.
//!
//! Codex ends a turn as soon as the model emits assistant text without a
//! `function_call`. Grok (and similar models) often do exactly that after
//! Continue. The proxy therefore:
//! - appends a loop instruction whenever tools are present
//! - forces `tool_choice=required` on Continue / prior one-liners
//! - holds `response.completed` and issues one follow-up nudge if the stream
//!   still finished without a tool call

use serde_json::{Value, json};

pub const AGENT_LOOP_INSTRUCTION: &str = "Codex ends the turn if you output only assistant text. If any inspection, command, or edit remains, you MUST emit a function_call in this same response. A one-line status or promise is not completion.";

pub const AGENT_LOOP_NUDGE: &str = "You ended this turn without a tool call. The previous assistant message is not completion. Immediately call the required tools. Do not output another status sentence.";

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
    if should_force_tools(body) {
        body.as_object_mut()
            .expect("object shape checked by caller")
            .insert("tool_choice".to_string(), json!("required"));
        append_developer_input(body, AGENT_LOOP_NUDGE);
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
pub fn build_compact_nudge_request(original: &Value) -> Value {
    let mut body = prepare_nudge_base(original);
    let continue_text = user_texts(original).into_iter().find(|text| {
        is_continue_nudge(text) || is_continue_nudge(&strip_environment_context(text))
    });
    if let Some(object) = body.as_object_mut() {
        object.insert("input".to_string(), json!([]));
    }
    if let Some(text) = continue_text {
        append_role_input(&mut body, "user", &text);
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
    if !request_has_tools(body) {
        return false;
    }
    should_force_nudge(body) || has_tool_result(body)
}

pub fn should_force_nudge(body: &Value) -> bool {
    continue_nudge_in_request(body)
        || last_role_text(body, "assistant")
            .as_deref()
            .is_some_and(is_status_one_liner)
}

pub fn has_tool_result(body: &Value) -> bool {
    let Some(Value::Array(items)) = body.get("input") else {
        return false;
    };
    items.iter().any(|item| {
        let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
        matches!(
            kind,
            "function_call_output" | "tool_result" | "custom_tool_call_output"
        ) || (item.get("call_id").is_some() && item.get("output").is_some())
    })
}

pub fn continue_nudge_in_request(body: &Value) -> bool {
    user_texts(body)
        .iter()
        .any(|text| is_continue_nudge(text) || is_continue_nudge(&strip_environment_context(text)))
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
    let chars = trimmed.chars().count();
    if chars > 80 || trimmed.lines().count() > 2 {
        return false;
    }
    let lower = trimmed.to_lowercase();
    STATUS_MARKERS
        .iter()
        .any(|marker| lower.contains(marker) || trimmed.contains(marker))
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

pub fn is_completed_event(event: &[u8]) -> bool {
    contains_seq(event, b"response.completed") || contains_seq(event, b"response.incomplete")
}

pub fn is_response_created_event(event: &[u8]) -> bool {
    contains_seq(event, b"response.created")
}

pub fn is_assistant_text_event(event: &[u8]) -> bool {
    if is_sse_comment(event) || is_completed_event(event) {
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
    if is_sse_comment(event) {
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

fn user_texts(body: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    match body.get("input") {
        Some(Value::String(text)) => texts.push(text.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                if !is_user_item(item) {
                    continue;
                }
                push_item_texts(item, &mut texts);
            }
        }
        _ => {}
    }
    texts
}

fn is_user_item(item: &Value) -> bool {
    if item.get("role").and_then(Value::as_str) == Some("user") {
        return true;
    }
    matches!(item.get("type").and_then(Value::as_str), Some("input_text"))
}

fn push_item_texts(item: &Value, texts: &mut Vec<String>) {
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        texts.push(text.to_string());
    }
    let content = item.get("content").unwrap_or(item);
    let combined = text_from_content(content);
    if !combined.is_empty() {
        texts.push(combined);
    }
    if let Value::Array(parts) = content {
        for part in parts {
            let part_text = text_from_content(part);
            if !part_text.is_empty() {
                texts.push(part_text);
            }
        }
    }
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
    if kind == "function_call" || kind.contains("function_call") {
        return true;
    }
    if value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        == Some("function_call")
    {
        return true;
    }
    value
        .pointer("/response/output")
        .and_then(Value::as_array)
        .is_some_and(|output| {
            output
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
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
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.last().unwrap()["role"], "developer");
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
    fn function_call_output_forces_tools() {
        let mut body = json!({
            "tools": [{"type": "function", "name": "exec_command"}],
            "previous_response_id": "resp_prev12345",
            "input": [{
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "TCP 0.0.0.0:3000 LISTENING"
            }]
        });
        assert_eq!(
            apply_agent_loop_guard(&mut body),
            AgentLoopGuard::ForceTools
        );
        assert_eq!(body["tool_choice"], "required");
        assert!(!should_force_nudge(&body));
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
        assert!(!is_status_one_liner("海鸥在线，你要整点薯条吗？"));
        assert!(!is_status_one_liner(
            "装完了。D 盘只剩 4.56GB，塞不下，已经落到 E 盘。Word / Excel 都能从开始菜单打开。"
        ));
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
    fn compact_nudge_keeps_original_previous_response_id_and_drops_history() {
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
        assert_eq!(nudged["previous_response_id"], "resp_prev12345");
        assert_eq!(nudged["tool_choice"], "required");
        let input = nudged["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "继续完成任务");
        assert_eq!(input[1]["role"], "developer");
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
}
