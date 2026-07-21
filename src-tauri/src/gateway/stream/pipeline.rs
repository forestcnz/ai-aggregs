//! 流式转换 trait 与组合工具。
//!
//! `StreamConverter` 是所有协议流转换器的统一接口（`IrStreamConverter` 实现）。
//!
//! 注：原 `Chained<A, B>` 组合器用于跨协议双跳（如 Anthropic→Responses 走 Chat 中转）。
//! IR 化后双跳消失（直接 src→IR→dst 单跳），已删除。

/// 流式事件转换器接口：上游每个 SSE event/data → 转换后若干条下游 payload。
pub trait StreamConverter: Send {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String>;
    fn on_done(&mut self) -> Vec<String>;
}

impl StreamConverter for Box<dyn StreamConverter> {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String> {
        (**self).on_event(event, data)
    }
    fn on_done(&mut self) -> Vec<String> {
        (**self).on_done()
    }
}

/// 同协议直通：原样转发。
pub(super) struct Noop;
impl StreamConverter for Noop {
    fn on_event(&mut self, _e: Option<&str>, data: &str) -> Vec<String> {
        vec![data.to_string()]
    }
    fn on_done(&mut self) -> Vec<String> {
        vec![]
    }
}
