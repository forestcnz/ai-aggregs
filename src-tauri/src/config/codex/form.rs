use serde::{Deserialize, Serialize};

/// 受管 provider（指向本网关的那一条）。id 同时作为顶层 `model_provider` 的值。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CodexProvider {
    /// provider id（如 `aggregs`），即 `model_providers.<id>` 的 key 与 `model_provider` 的值
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 网关 consumer key 直接写入（开箱即用；与 env_key/requires_openai_auth 互斥）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental_bearer_token: Option<String>,
}

/// 表单管理的 Codex 配置子集。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CodexForm {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: CodexProvider,
    /// 加载时的受管 provider id（前端原样回传、不可编辑）。
    /// 保存时若与 `provider.id` 不同，则删除旧 `[model_providers.<旧id>]`（改名清理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_provider_id: Option<String>,
    /// 是否在 config.toml 设 `model_catalog_json`（启用 /model 模型目录）。
    /// form-only，不写入 config.toml；由 config.toml 是否已含该 key 推断。
    #[serde(default)]
    pub enable_model_catalog: bool,
    /// 模型目录的手动清单（form-only；持久化于 catalog 文件，load 时回填）。
    /// 保存时若开启目录，则克隆内置模板为每个模型生成一个 catalog 条目。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catalog_models: Vec<String>,
}

/// load 返回的复合结构。
#[derive(Serialize)]
pub struct CodexLoadResult {
    /// 解析后的配置文件绝对路径
    pub path: String,
    /// 文件是否存在（不存在时 form 为默认空壳）
    pub exists: bool,
    /// 原文件是否含 `#` 注释（前端据此提示「注释将丢失」）
    pub has_comments: bool,
    /// 表单数据
    pub form: CodexForm,
}

/// save 返回的复合结构（前端据此 toast 模型目录生成结果）。
#[derive(Serialize, Debug)]
pub struct CodexSaveResult {
    /// 模型目录是否成功生成并写入
    pub catalog_ok: bool,
    /// 生成的 catalog 条目数（0 = 未启用或失败）
    pub catalog_count: usize,
    /// 目录未生成时的原因（未启用时为 None）
    pub catalog_error: Option<String>,
}
