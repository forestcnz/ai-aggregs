//! 流式转换 trait 与组合工具。
//!
//! `StreamConverter` 是所有协议流转换器（如 `AnthropicToChatStream`）的统一接口，
//! `Chained` 用于跨协议双跳（如 Anthropic→Responses 走 Chat 中转），
//! `Noop` 用于同协议直通场景。

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

/// 串联两个 converter：A 的输出作为 B 的输入。
pub(super) struct Chained<A: StreamConverter, B: StreamConverter>(pub(super) A, pub(super) B);

impl<A: StreamConverter, B: StreamConverter> StreamConverter for Chained<A, B> {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String> {
        let mid = self.0.on_event(event, data);
        let mut out = Vec::new();
        for p in mid {
            out.extend(self.1.on_event(None, &p));
        }
        out
    }
    fn on_done(&mut self) -> Vec<String> {
        let mid = self.0.on_done();
        let mut out = Vec::new();
        for p in mid {
            out.extend(self.1.on_event(None, &p));
        }
        out.extend(self.1.on_done());
        out
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
