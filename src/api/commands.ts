/**
 * Tauri IPC 命令封装 + 类型定义
 *
 * 所有类型与 `src-tauri/src/` 中的 Rust 结构体一一对应，
 * 通过 Tauri 的 `invoke()` 机制实现前后端通信。
 */
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

// ===================== 类型（与 Rust 结构体对应） =====================

/** 上游协议类型，一个 Provider 固定一种 */
export type Protocol = 'chat' | 'responses' | 'anthropic'

/**
 * API Key 条目（untagged enum：对象或纯字符串）。
 * - 旧格式：`"sk-xxx"`（enabled 默认 true）
 * - 新格式：`{ key: "sk-xxx", enabled: true }`
 * 使用 `normalizeKey()` 统一为对象格式。
 */
export type ApiKeyEntry = { key: string; enabled: boolean } | string

/** 提供商配置 */
export interface ProviderConfig {
  id: number
  name: string
  protocol: Protocol
  base_url: string
  api_keys: ApiKeyEntry[]
  models: string[]
  timeout_secs: number
  extra_headers: Record<string, string>
  enabled: boolean
  /** 固定思考强度（注入到发给上游的请求体），null 表示不注入 */
  reasoning_effort?: string | null
}

/** 模型映射：把对外别名重定向到一组实际后端模型（负载均衡 / 故障转移） */
export interface ModelMapping {
  /** 对外别名 — 用户请求时填的模型名 */
  alias: string
  /** 实际后端模型池（按顺序尝试） */
  models: string[]
  enabled: boolean
}

/** 全局配置 */
export interface Config {
  listen: string
  providers: ProviderConfig[]
  consumer: {
    api_keys: string[]
    /** 自动从已启用 provider 的 models 并集计算（含已启用别名），不应手动设置 */
    models: string[]
  }
  log: { level: string }
  /** key 429 后加入黑名单的时长（秒），默认 600 */
  key_blacklist_secs: number
  /** 启动应用时是否恢复上次网关运行状态 */
  auto_start_gateway: boolean
  /** 模型映射（别名 → 实际后端模型池） */
  model_mappings: ModelMapping[]
}

/** 网关运行状态 */
export interface GatewayStatus {
  running: boolean
  listen_addr: string
}

/** 单个 key 的运行时状态快照 */
export interface KeyStatus {
  idx: number
  masked: string
  enabled: boolean
  blacklisted: boolean
  /** 黑名单剩余秒数，null 表示未被拉黑 */
  blacklist_remaining_secs?: number | null
}

/** Provider 运行时状态（含 key 黑名单信息） */
export interface ProviderRuntime {
  name: string
  enabled: boolean
  protocol: string
  base_url: string
  models: string[]
  keys: KeyStatus[]
}

/** 日志条目（前端事件 `gateway-log` 的 payload） */
export interface LogEntry {
  level: string
  target: string
  message: string
  /** 事件发生时的 UNIX 时间戳（毫秒） */
  ts: number
}

/** 单个模型的聚合用量行 */
export interface UsageModelRow {
  model: string
  requests: number
  input_tokens: number
  output_tokens: number
  total_tokens: number
}

/** 用量统计汇总（含各模型明细 + 总计） */
export interface UsageSummary {
  models: UsageModelRow[]
  total_requests: number
  total_input_tokens: number
  total_output_tokens: number
  total_tokens: number
}

// ===================== 命令封装 =====================

/** 获取当前配置（返回时自动同步 consumer.models） */
export const getConfig = () => invoke<Config>('get_config')

/** 保存配置（持久化 + 热更新日志级别 + 网关重建） */
export const saveConfig = (cfg: Config) => invoke<void>('save_config', { cfg })

/** 启动网关，返回监听地址 */
export const startGateway = () => invoke<string>('start_gateway')

/**
 * 页面就绪后按配置自动恢复网关：仅当 `auto_start_gateway` 且上次退出时网关在运行才启动。
 * 返回是否实际启动了网关。由 App 在 ready 后调用。
 */
export const autostartGatewayIfConfigured = () =>
  invoke<boolean>('autostart_gateway_if_configured')

/** 停止网关 */
export const stopGateway = () => invoke<void>('stop_gateway')

/** 查询网关运行状态 */
export const gatewayStatus = () => invoke<GatewayStatus>('gateway_status')

/** 切换 provider 启用/禁用 */
export const toggleProvider = (name: string, enabled: boolean) =>
  invoke<void>('toggle_provider', { name, enabled })

/** 切换单个 API Key 的启用/禁用 */
export const toggleKey = (providerName: string, keyIdx: number, enabled: boolean) =>
  invoke<void>('toggle_key', { providerName, keyIdx, enabled })

/** 查询所有 provider 的运行时状态 */
export const runtimeStatus = () => invoke<ProviderRuntime[]>('runtime_status')

/** 启用开机自启 */
export const enableAutostart = () => invoke<void>('enable_autostart')

/** 禁用开机自启 */
export const disableAutostart = () => invoke<void>('disable_autostart')

/** 查询开机自启状态 */
export const autostartStatus = () => invoke<boolean>('autostart_status')

/**
 * 查询用量统计。
 * @param consumerKey 指定 consumer key，null 查全部
 * @param days 时间范围天数，null 查全部
 */
export const getUsage = (consumerKey: string | null, days: number | null) =>
  invoke<UsageSummary>('get_usage', { consumerKey, days })

/**
 * 查询供应商用量统计。
 * @param providerId 供应商 ID，null 查全部
 * @param providerKey 指定 key，null 查全部
 * @param days 时间范围天数，null 查全部
 */
export const getProviderUsage = (
  providerId: number | null,
  providerKey: string | null,
  days: number | null
) => invoke<UsageSummary>('get_provider_usage', { providerId, providerKey, days })

/**
 * 查询各别名上次成功响应的实际模型（内存记录）。
 * 返回 别名 → 实际模型 的映射，用于设置页高亮当前命中的后端模型。
 */
export const getLastUsedModels = () => invoke<Record<string, string>>('last_used_models')

// ===================== OpenCode 配置编辑 =====================

/** OpenCode model 的 modalities 配置 */
export interface OcModalities {
  input: string[]
  output: string[]
}

/** OpenCode model 的 token 限制 */
export interface OcLimit {
  context: number
  output: number
}

/** OpenCode model 配置 */
export interface OcModel {
  id: string
  name?: string | null
  attachment: boolean
  reasoning: boolean
  tool_call: boolean
  modalities: OcModalities
  limit?: OcLimit | null
}

/** OpenCode provider 的 options 段 */
export interface OcOptions {
  baseURL?: string | null
  apiKey?: string | null
}

/** OpenCode provider 配置（id 对应 model 字段前缀） */
export interface OcProvider {
  id: string
  name?: string | null
  /** npm 包名，决定协议 SDK（下拉项） */
  npm?: string | null
  options: OcOptions
  models: OcModel[]
}

/** OpenCode 配置表单（仅管理 model / small_model / default_agent / provider / disabled_providers） */
export interface OcForm {
  model?: string | null
  small_model?: string | null
  default_agent?: string | null
  providers: OcProvider[]
  /** 被屏蔽的 provider id 列表（opencode 顶层 disabled_providers） */
  disabled_providers?: string[]
}

/** opencode_config_load 返回结构 */
export interface OcLoadResult {
  /** 配置文件绝对路径 */
  path: string
  /** 文件是否存在（不存在时 form 为空默认值） */
  exists: boolean
  /** 原文件是否含 JSONC 注释（前端据此提示「注释将丢失」） */
  has_comments: boolean
  /** 表单数据 */
  form: OcForm
}

/** 从网关同步的模式 */
export type OcSyncMode = 'append' | 'overwrite' | 'models'

/** 读取并解析 opencode 配置文件，提取表单字段 */
export const opencodeConfigLoad = () =>
  invoke<OcLoadResult>('opencode_config_load')

/** 把表单按 key 合并写回配置文件（保存前自动备份 .bak） */
export const opencodeConfigSave = (form: OcForm) =>
  invoke<void>('opencode_config_save', { form })

/** 执行 `opencode models` 获取 opencode 可用的 provider id 列表（屏蔽下拉候选） */
export const opencodeProviderIds = () => invoke<string[]>('opencode_provider_ids')

/** 执行 `opencode -v` 获取版本号；未安装返回 null（控制侧边栏入口显示） */
export const opencodeVersion = () => invoke<string | null>('opencode_version')

// ===================== Claude Code 配置编辑 =====================

/** Claude Code settings.json 中的一条环境变量（secret 仅为前端展示提示） */
export interface CcEnvEntry {
  key: string
  value: string
  /** 是否敏感凭证（含 TOKEN/SECRET/PASSWORD），前端据此掩码展示 */
  secret?: boolean
}

/** Claude Code 配置表单（仅管理 settings.json 的 env 段） */
export interface CcForm {
  env: CcEnvEntry[]
}

/** claude_code_config_load 返回结构 */
export interface CcLoadResult {
  /** 配置文件绝对路径 */
  path: string
  /** 文件是否存在（不存在时 form 为空默认值） */
  exists: boolean
  /** 表单数据 */
  form: CcForm
}

/** 读取并解析 Claude Code 配置文件，提取 env 段 */
export const claudeCodeConfigLoad = () => invoke<CcLoadResult>('claude_code_config_load')

/** 把表单的 env 段整体合并写回配置文件（保存前自动备份 .bak） */
export const claudeCodeConfigSave = (form: CcForm) =>
  invoke<void>('claude_code_config_save', { form })

/** 执行 `claude --version` 获取版本号；未安装返回 null（控制侧边栏入口显示） */
export const claudeCodeVersion = () => invoke<string | null>('claude_code_version')

// ===================== Codex 配置编辑 =====================

/**
 * Codex（~/.codex/config.toml）受管 provider — 指向本网关的那一条。
 * id 同时作为顶层 model_provider 的值；base_url 指到网关 /v1（Codex 走 Responses 协议）。
 */
export interface CodexProvider {
  id: string
  name?: string | null
  base_url?: string | null
  /** 网关 consumer key 直接写入（开箱即用；与 env_key 互斥） */
  experimental_bearer_token?: string | null
}

/** Codex 配置表单（管理 model + 一条受管 provider + 模型目录清单） */
export interface CodexForm {
  model?: string | null
  provider: CodexProvider
  /** 加载时的受管 provider id（前端原样回传、不可编辑；改名时后端清理旧表） */
  loaded_provider_id?: string | null
  /** 是否在 config.toml 设 model_catalog_json（启用 /model 模型目录） */
  enable_model_catalog?: boolean
  /** 模型目录手动清单（持久化于 catalog 文件，load 时回填） */
  catalog_models?: string[]
}

/** codex_config_save 返回结构（前端据此 toast 模型目录生成结果） */
export interface CodexSaveResult {
  /** 模型目录是否成功生成并写入 */
  catalog_ok: boolean
  /** 生成的 catalog 条目数（0 = 未启用或失败） */
  catalog_count: number
  /** 目录未生成时的原因（未启用时为 null） */
  catalog_error: string | null
}

/** codex_config_load 返回结构 */
export interface CodexLoadResult {
  /** 配置文件绝对路径 */
  path: string
  /** 文件是否存在（不存在时 form 为默认空壳） */
  exists: boolean
  /** 原文件是否含 TOML 注释（前端据此提示「注释将丢失」） */
  has_comments: boolean
  /** 表单数据 */
  form: CodexForm
}

/** 读取并解析 Codex 配置文件，提取受管字段 */
export const codexConfigLoad = () => invoke<CodexLoadResult>('codex_config_load')

/** 把表单按 key 合并写回配置文件（保存前自动备份 .bak）。
 * 若开启模型目录且清单非空，后端克隆内置模板生成 catalog 并设 model_catalog_json。 */
export const codexConfigSave = (form: CodexForm) =>
  invoke<CodexSaveResult>('codex_config_save', { form })

/** 执行 `codex --version` 获取版本号；未安装返回 null（控制侧边栏入口显示） */
export const codexVersion = () => invoke<string | null>('codex_version')

// ===================== 事件监听 =====================

/** 监听网关日志事件，返回取消监听函数 */
export function onLog(callback: (entry: LogEntry) => void): Promise<UnlistenFn> {
  return listen<LogEntry>('gateway-log', (e) => callback(e.payload))
}

/** 监听网关状态变化事件，返回取消监听函数 */
export function onGatewayStateChanged(callback: (running: boolean) => void): Promise<UnlistenFn> {
  return listen<boolean>('gateway-state-changed', (e) => callback(e.payload))
}

// ===================== 工具函数 =====================

/** 把 ApiKeyEntry 统一为 {key, enabled} 格式（兼容旧的纯字符串） */
export function normalizeKey(entry: ApiKeyEntry): { key: string; enabled: boolean } {
  if (typeof entry === 'string') {
    return { key: entry, enabled: true }
  }
  return entry
}

/**
 * 统一的密钥掩码函数（与后端 provider.rs::mask_key 行为一致）。
 * 短 key 也保证首尾都露一部分：
 *   - len <= 2  → 首1 + "**"
 *   - len <= 6  → 首1 + "**" + 尾1
 *   - len <= 12 → 首3 + "**" + 尾3
 *   - len > 12  → 首6 + "**" + 尾6
 * 全程按 Unicode code point 切分，emoji / 多字节字符不会切坏。
 */
export function maskKey(key: string): string {
  // 使用 Array.from 按 code point 拆分，避免 JS 字符串按 UTF-16 码元切到 emoji 中间
  const chars = Array.from(key)
  const len = chars.length
  if (len <= 2) {
    return chars[0] + '**'
  }
  const [hl, tl] = len <= 6 ? [1, 1] : len <= 12 ? [3, 3] : [6, 6]
  const head = chars.slice(0, hl).join('')
  const tail = chars.slice(len - tl).join('')
  return `${head}**${tail}`
}
