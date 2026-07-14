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
  name: string
  protocol: Protocol
  base_url: string
  api_keys: ApiKeyEntry[]
  models: string[]
  timeout_secs: number
  max_retries: number
  extra_headers: Record<string, string>
  enabled: boolean
  /** 固定思考强度（注入到发给上游的请求体），null 表示不注入 */
  reasoning_effort?: string | null
}

/** 全局配置 */
export interface Config {
  listen: string
  providers: ProviderConfig[]
  consumer: {
    api_keys: string[]
    /** 自动从已启用 provider 的 models 并集计算，不应手动设置 */
    models: string[]
  }
  log: { level: string }
  /** key 429 后加入黑名单的时长（秒），默认 600 */
  key_blacklist_secs: number
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

// ===================== 命令封装 =====================

/** 获取当前配置（返回时自动同步 consumer.models） */
export const getConfig = () => invoke<Config>('get_config')

/** 保存配置（持久化 + 热更新日志级别 + 网关重建） */
export const saveConfig = (cfg: Config) => invoke<void>('save_config', { cfg })

/** 启动网关，返回监听地址 */
export const startGateway = () => invoke<string>('start_gateway')

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
