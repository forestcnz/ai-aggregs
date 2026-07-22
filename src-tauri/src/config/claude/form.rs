use serde::{Deserialize, Serialize};

/// 表单管理的 Claude Code 配置子集。
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct CcForm {
    #[serde(default)]
    pub env: Vec<EnvEntry>,
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

/// 环境变量条目。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<bool>,
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
