// Tauri IPC 命令封装 + 类型定义
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

// ===================== 类型（与 Rust 结构体对应）=====================

export type Protocol = 'chat' | 'responses' | 'anthropic'

/** API Key 条目（untagged enum：对象或纯字符串） */
export type ApiKeyEntry = { key: string; enabled: boolean } | string

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
  reasoning_effort?: string | null
}

export interface Config {
  listen: string
  providers: ProviderConfig[]
  consumer: {
    api_keys: string[]
    models: string[]
  }
  log: { level: string }
  key_blacklist_secs: number
}

export interface GatewayStatus {
  running: boolean
  listen_addr: string
}

export interface KeyStatus {
  idx: number
  masked: string
  enabled: boolean
  blacklisted: boolean
  blacklist_remaining_secs?: number | null
}

export interface ProviderRuntime {
  name: string
  enabled: boolean
  protocol: string
  base_url: string
  models: string[]
  keys: KeyStatus[]
}

export interface LogEntry {
  level: string
  target: string
  message: string
  /** 事件发生时的 UNIX 时间戳（毫秒） */
  ts: number
}

// ===================== 命令封装 =====================

export const getConfig = () => invoke<Config>('get_config')
export const saveConfig = (cfg: Config) => invoke<void>('save_config', { cfg })
export const startGateway = () => invoke<string>('start_gateway')
export const stopGateway = () => invoke<void>('stop_gateway')
export const gatewayStatus = () => invoke<GatewayStatus>('gateway_status')
export const toggleProvider = (name: string, enabled: boolean) =>
  invoke<void>('toggle_provider', { name, enabled })
export const toggleKey = (providerName: string, keyIdx: number, enabled: boolean) =>
  invoke<void>('toggle_key', { providerName, keyIdx, enabled })
export const runtimeStatus = () => invoke<ProviderRuntime[]>('runtime_status')
export const enableAutostart = () => invoke<void>('enable_autostart')
export const disableAutostart = () => invoke<void>('disable_autostart')
export const autostartStatus = () => invoke<boolean>('autostart_status')

// ===================== 事件监听 =====================

export function onLog(callback: (entry: LogEntry) => void): Promise<UnlistenFn> {
  return listen<LogEntry>('gateway-log', (e) => callback(e.payload))
}

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
