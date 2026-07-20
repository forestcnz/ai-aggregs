//! 流式 Chat → Anthropic 转换器。
//!
//! 上游发 OpenAI Chat 流式 chunk（含 delta.content / delta.tool_calls / finish_reason），
//! 转为 Anthropic SSE 事件（message_start / content_block_start/delta/stop / message_delta / message_stop）。
//!
//! 处理三类边界 case：
//! 1. 乱序到达（DeepSeek/GLM/Zhipu）：tool_call id 先到、name 后到时缓存 arguments。
//! 2. late starts 兜底：finish_reason 时仍无 name 则用 fallback 名宣告，避免参数丢失。
//! 3. Copilot 无限空白 bug：跟踪连续空白字符数，超过阈值中止 tool 流。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::gateway::converter::{map_finish_reason_chat_to_anthropic, rand_id};
use crate::gateway::stream::pipeline::StreamConverter;

/// 单个 tool_call 的累积状态，按上游 chat tool_call index 索引。
///
/// 解决的边界 case：
/// 1. **乱序到达**（DeepSeek/GLM/Zhipu）：上游先发 id+arguments，name 后到。
///    未到位前缓存在 `pending_args`，name 到达后再发 content_block_start。
/// 2. **多 tool_call 并发**：上游可在不同 chunk 发多个 index 的 delta。
/// 3. **late starts 兜底**：name 永远没到时，finish_reason 触发 fallback 宣告。
/// 4. **Copilot 无限空白 bug**：GitHub Copilot 有时在 tool_call arguments 中
///    产生无限连续的空白字符（换行/空格），导致客户端卡死。
///    `consecutive_whitespace` 跟踪连续空白字符数，超过阈值时中止该 tool 流。
const INFINITE_WHITESPACE_THRESHOLD: usize = 500;

#[derive(Default)]
struct ToolBlockState {
    /// Anthropic 端 content_block 的 index（宣告后固定）
    anthropic_index: Option<usize>,
    /// 上游 tool_call id（可能跨多 chunk 累积）
    id: String,
    /// 上游 tool_call name（可能跨多 chunk 累积）
    name: String,
    /// name 到达前的 arguments 缓冲，宣告时一次性 flush
    pending_args: String,
    /// 是否已发 content_block_start
    announced: bool,
    /// 连续空白字符计数（Copilot 无限空白 bug 检测）
    consecutive_whitespace: usize,
    /// 是否因无限空白 bug 被中止
    aborted: bool,
}

pub(super) struct ChatToAnthropicStream {
    started: bool,
    sent_done: bool,
    next_block: usize,
    cur_block: Option<(usize, String)>,
    /// 上游（chat）在 finish_reason 帧发来的 reasoning_signature；
    /// 关闭 thinking 块前用它发 signature_delta，保证多轮 thinking 完整性
    pending_signature: Option<String>,
    /// 按 chat tool_call index 索引的累积状态。支持 DeepSeek 等乱序上游。
    tool_blocks: HashMap<usize, ToolBlockState>,
    /// 当前 tool_use 块是否有 input_json_delta（用于空 args 补 "{}"）
    cur_tool_had_delta: bool,
}

impl ChatToAnthropicStream {
    pub(super) fn new() -> Self {
        Self {
            started: false,
            sent_done: false,
            next_block: 0,
            cur_block: None,
            pending_signature: None,
            tool_blocks: HashMap::new(),
            cur_tool_had_delta: false,
        }
    }

    /// 关闭当前 block；若是 thinking 块且有累积的 signature，
    /// 在 content_block_stop 之前先发 signature_delta 事件。
    ///
    /// 若是 tool_use 块且无任何 input_json_delta，主动补一个 "{}"——
    /// Claude SDK 等严格客户端只从 delta 累积 input，null input 会导致后续工具执行失败。
    fn close_cur_block(&mut self, out: &mut Vec<String>) {
        if let Some((idx, ty)) = self.cur_block.take() {
            if ty == "thinking" {
                if let Some(sig) = self.pending_signature.take() {
                    out.push(signature_delta_event(idx, &sig));
                }
            }
            if ty == "tool_use" && !self.cur_tool_had_delta {
                // 空 arguments 补 "{}"，避免客户端收到 null input
                out.push(input_json_delta_event(idx, "{}"));
            }
            self.cur_tool_had_delta = false;
            out.push(content_block_stop_frame(idx));
        }
    }

    fn ensure_text(&mut self, out: &mut Vec<String>) -> usize {
        if let Some((idx, ref ty)) = self.cur_block {
            if ty == "text" {
                return idx;
            }
            self.close_cur_block(out);
        }
        self.cur_block.take();
        let idx = self.next_block;
        self.next_block += 1;
        self.cur_block = Some((idx, "text".into()));
        out.push(content_block_start_text_frame(idx));
        idx
    }

    fn ensure_thinking(&mut self, out: &mut Vec<String>) -> usize {
        if let Some((idx, ref ty)) = self.cur_block {
            if ty == "thinking" {
                return idx;
            }
            self.close_cur_block(out);
        }
        self.cur_block.take();
        let idx = self.next_block;
        self.next_block += 1;
        self.cur_block = Some((idx, "thinking".into()));
        out.push(content_block_start_thinking_frame(idx));
        idx
    }

    /// finish_reason 触发时的 late starts 兜底：
    /// 上游先发 arguments 但 name 永远没到（极端边界 case），
    /// 用 fallback 名宣告，避免参数数据丢失。
    fn finalize_pending_tool_blocks(&mut self, out: &mut Vec<String>) {
        let mut late_starts: Vec<(usize, String, String, String)> = Vec::new();
        for (chat_idx, state) in self.tool_blocks.iter_mut() {
            if state.announced || state.aborted {
                continue;
            }
            // 完全空的状态跳过（理论上不会出现在 map 中，但防御性判断）
            if state.pending_args.is_empty() && state.id.is_empty() && state.name.is_empty() {
                continue;
            }
            let bidx = self.next_block;
            self.next_block += 1;
            state.anthropic_index = Some(bidx);
            state.announced = true;
            let fallback_id = if state.id.is_empty() {
                format!("tool_call_{chat_idx}")
            } else {
                state.id.clone()
            };
            let fallback_name = if state.name.is_empty() {
                "unknown_tool".to_string()
            } else {
                state.name.clone()
            };
            let pending = std::mem::take(&mut state.pending_args);
            late_starts.push((bidx, fallback_id, fallback_name, pending));
        }
        // 按 anthropic_index 排序保证输出顺序稳定
        late_starts.sort_unstable_by_key(|(idx, _, _, _)| *idx);
        for (bidx, id, name, pending) in late_starts {
            out.push(content_block_start_tool_event(bidx, json!(id), json!(name)));
            if !pending.is_empty() {
                out.push(input_json_delta_event(bidx, &pending));
            } else {
                // 兜底：无 arguments 也补 "{}"
                out.push(input_json_delta_event(bidx, "{}"));
            }
            out.push(content_block_stop_frame(bidx));
        }
    }
}

impl StreamConverter for ChatToAnthropicStream {
    fn on_event(&mut self, _event: Option<&str>, data: &str) -> Vec<String> {
        if data == "[DONE]" {
            let mut out = vec![];
            self.close_cur_block(&mut out);
            // 上游 [DONE] 前若仍有未宣告的 tool_block，兜底宣告
            self.finalize_pending_tool_blocks(&mut out);
            out.push(message_stop_event());
            self.sent_done = true;
            return out;
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let mut out: Vec<String> = vec![];

        if !self.started {
            self.started = true;
            out.push(message_start_event());
        }

        let choice = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        let Some(choice) = choice else {
            return out;
        };
        let delta = choice.get("delta").cloned().unwrap_or(json!({}));

        // 累积上游 chat 的 reasoning_signature（finish_reason 那一帧才出现）
        if let Some(sig) = delta.get("reasoning_signature").and_then(|x| x.as_str()) {
            if !sig.is_empty() {
                self.pending_signature = Some(sig.to_string());
            }
        }

        if let Some(rc) = delta.get("reasoning_content") {
            if let Some(t) = rc.as_str() {
                if !t.is_empty() {
                    let idx = self.ensure_thinking(&mut out);
                    out.push(thinking_delta_event(idx, t));
                }
            }
        }

        if let Some(content) = delta.get("content") {
            if let Some(t) = content.as_str() {
                if !t.is_empty() {
                    let idx = self.ensure_text(&mut out);
                    out.push(text_delta_event(idx, t));
                }
            }
        }

        // tool_calls 处理：支持乱序到达（id/name/arguments 可任意顺序）
        if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for tc in tcs {
                let chat_idx = tc
                    .get("index")
                    .and_then(|i| i.as_u64())
                    .map(|i| i as usize)
                    .unwrap_or(0);

                // 取或创建状态（按 chat tool_call index 索引，支持多并发）
                // 在作用域内完成累积 + 决策，避免 self 借用冲突
                let (announce_action, emit_action) = {
                    let state = self
                        .tool_blocks
                        .entry(chat_idx)
                        .or_default();

                    // Copilot 无限空白 bug：已中止的 tool_call 跳过所有后续处理
                    if state.aborted {
                        (None, None)
                    } else {
                        // 累积 id（可能跨多 chunk 到达）
                        if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                            state.id = id.to_string();
                        }
                        // 累积 name（DeepSeek/GLM 可能比 id/arguments 后到）
                        if let Some(name) = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                        {
                            state.name = name.to_string();
                        }
                        // 累积 arguments（无论是否宣告，先入 pending_args）
                        if let Some(args) = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                        {
                            if !args.is_empty() {
                                // Copilot 无限空白 bug 检测：跟踪连续空白字符
                                for ch in args.chars() {
                                    if ch.is_whitespace() {
                                        state.consecutive_whitespace += 1;
                                    } else {
                                        state.consecutive_whitespace = 0;
                                    }
                                }
                                if state.consecutive_whitespace >= INFINITE_WHITESPACE_THRESHOLD {
                                    tracing::warn!(
                                        chat_idx,
                                        name = %state.name,
                                        consecutive_ws = state.consecutive_whitespace,
                                        "Copilot 无限空白 bug 检测：中止 tool_call 流"
                                    );
                                    state.aborted = true;
                                    state.pending_args.clear();
                                } else {
                                    state.pending_args.push_str(args);
                                }
                            }
                        }

                        // 决策（aborted 可能刚被上面的空白检测置位）
                        if state.aborted {
                            (None, None)
                        } else if !state.announced
                            && !state.id.is_empty()
                            && !state.name.is_empty()
                        {
                            let pending = std::mem::take(&mut state.pending_args);
                            (
                                Some((state.id.clone(), state.name.clone(), pending)),
                                None,
                            )
                        } else if state.announced && !state.pending_args.is_empty() {
                            let args = std::mem::take(&mut state.pending_args);
                            (None, Some((state.anthropic_index, args)))
                        } else {
                            (None, None)
                        }
                    }
                };

                // 执行宣告（已释放 state 借用，可自由调用 self 方法）
                if let Some((id, name, pending)) = announce_action {
                    self.close_cur_block(&mut out);
                    let bidx = self.next_block;
                    self.next_block += 1;
                    if let Some(state) = self.tool_blocks.get_mut(&chat_idx) {
                        state.anthropic_index = Some(bidx);
                        state.announced = true;
                    }
                    self.cur_block = Some((bidx, "tool_use".into()));
                    self.cur_tool_had_delta = false;

                    out.push(content_block_start_tool_event(bidx, json!(id), json!(name)));

                    // 宣告时 flush pending_args（DeepSeek 场景：arguments 在 name 之前已到）
                    if !pending.is_empty() {
                        out.push(input_json_delta_event(bidx, &pending));
                        self.cur_tool_had_delta = true;
                    }
                } else if let Some((Some(bidx), args)) = emit_action {
                    // 已宣告的 tool_block 直接发 input_json_delta
                    out.push(input_json_delta_event(bidx, &args));
                    self.cur_tool_had_delta = true;
                }
            }
        }

        if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            self.close_cur_block(&mut out);
            // 兜底：name 永远没到的工具（极端边界 case）
            self.finalize_pending_tool_blocks(&mut out);
            let stop_reason = map_finish_reason_chat_to_anthropic(fr);
            let mut usage = json!({});
            if let Some(u) = v.get("usage") {
                if let Some(pt) = u.get("prompt_tokens") {
                    usage["input_tokens"] = pt.clone();
                }
                if let Some(ct) = u.get("completion_tokens") {
                    usage["output_tokens"] = ct.clone();
                }
            }
            out.push(message_delta_event(stop_reason, usage));
        }

        out
    }

    fn on_done(&mut self) -> Vec<String> {
        if self.sent_done {
            return vec![];
        }
        self.sent_done = true;
        let mut out = vec![];
        self.close_cur_block(&mut out);
        out.push(message_stop_event());
        out
    }
}

// ---- Chat -> Anthropic 事件构造 helper ----

fn message_start_event() -> String {
    json!({
        "type":"message_start",
        "message":{
            "id": format!("msg_{}", rand_id()),
            "type":"message","role":"assistant","model":"",
            "content":[],"stop_reason":null,
            "usage":{"input_tokens":0,"output_tokens":0}
        }
    })
    .to_string()
}
fn content_block_start_text_frame(idx: usize) -> String {
    json!({
        "type":"content_block_start","index":idx,
        "content_block":{"type":"text","text":""}
    })
    .to_string()
}
fn content_block_start_thinking_frame(idx: usize) -> String {
    json!({
        "type":"content_block_start","index":idx,
        "content_block":{"type":"thinking","thinking":"","signature":""}
    })
    .to_string()
}
fn thinking_delta_event(idx: usize, text: &str) -> String {
    json!({
        "type":"content_block_delta","index":idx,
        "delta":{"type":"thinking_delta","thinking":text}
    })
    .to_string()
}
/// thinking 块的签名增量；客户端在多轮 thinking 中必须把它和 thinking 一起回传
fn signature_delta_event(idx: usize, signature: &str) -> String {
    json!({
        "type":"content_block_delta","index":idx,
        "delta":{"type":"signature_delta","signature":signature}
    })
    .to_string()
}
fn content_block_start_tool_event(idx: usize, id: Value, name: Value) -> String {
    json!({
        "type":"content_block_start","index":idx,
        "content_block":{"type":"tool_use","id":id,"name":name,"input":{}}
    })
    .to_string()
}
fn text_delta_event(idx: usize, text: &str) -> String {
    json!({
        "type":"content_block_delta","index":idx,
        "delta":{"type":"text_delta","text":text}
    })
    .to_string()
}
fn input_json_delta_event(idx: usize, partial: &str) -> String {
    json!({
        "type":"content_block_delta","index":idx,
        "delta":{"type":"input_json_delta","partial_json":partial}
    })
    .to_string()
}
fn content_block_stop_frame(idx: usize) -> String {
    json!({"type":"content_block_stop","index":idx}).to_string()
}
fn message_delta_event(stop_reason: String, usage: Value) -> String {
    json!({
        "type":"message_delta",
        "delta":{"stop_reason":stop_reason,"stop_sequence":null},
        "usage":usage
    })
    .to_string()
}
fn message_stop_event() -> String {
    json!({"type":"message_stop"}).to_string()
}
