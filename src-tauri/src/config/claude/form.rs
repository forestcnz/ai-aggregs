use serde::{Deserialize, Serialize};

/// 表单管理的 Claude Code 配置子集。
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct CcForm {
    /// Base URL（对应 `ANTHROPIC_BASE_URL`）。空 = 使用 Claude Code 默认值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Auth Token（对应 `ANTHROPIC_AUTH_TOKEN`）。空 = 使用已有配置/token 文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    /// 主模型（对应 `ANTHROPIC_MODEL`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 小/快速模型（对应 `ANTHROPIC_SMALL_FAST_MODEL`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_fast_model: Option<String>,
    /// 各代际默认模型
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_haiku: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_sonnet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_opus: Option<String>,
    /// sub-agent 模型
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_model: Option<String>,
    /// 未知 env 条目（表单中未直接展示的补充环境变量），保存时原样带回。
    /// 含 `extra_env` 与原始加载的 `raw_json`。
    #[serde(default)]
    pub extra_env: Vec<ExtraEnvEntry>,
    /// 文件是否存在
    #[serde(default, skip_serializing_if = "is_false")]
    pub file_exists: bool,
    /// 上一次完整 JSON 文本（用于 diff/备份提示，不写入文件）
    #[serde(skip)]
    pub raw_json: String,
}

fn is_false(v: &bool) -> bool {
    !v
}

/// 额外的 env 条目（表单未直接管理的自定义环境变量）。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExtraEnvEntry {
    pub key: String,
    pub value: String,
}

/// load 返回的复合结构。
#[derive(Serialize)]
pub struct CcLoadResult {
    /// 解析后的配置文件绝对路径
    pub path: String,
    /// 文件是否存在（不存在时 form 为空默认值）
    pub exists: bool,
    /// 表单数据
    pub form: CcForm,
}
