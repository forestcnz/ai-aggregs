# ai-aggregs — Agent 指南

## 项目概述

AI API 聚合网关桌面应用。通过一个本地 HTTP 端点反向代理多个 AI 提供商（OpenAI/Anthropic），支持协议转换和故障转移。**Tauri v2** 壳 + **Vue 3** 前端 + **Rust/Axum** 网关嵌入在同一进程中。

## 包管理器

- **JS 依赖用 Bun**，不要用 npm/yarn/pnpm
- Rust 依赖用 `src-tauri/` 下的标准 Cargo

## 关键命令

| 用途 | 命令 | 说明 |
|------|------|------|
| 完整应用开发 | `bun run tauri dev` | 启动 Vite 热重载 + Tauri 窗口 |
| 前端开发 | `bun run dev` | 仅 Vite 开发服务器（端口 1420），无 Tauri |
| 前端类型检查+构建 | `bun run build` | `vue-tsc --noEmit && vite build`，**不会**构建 Tauri 安装包 |
| Tauri 安装包构建 | `bun run tauri build` | 构建完整桌面安装包（MSI/NSIS/DMG） |
| Rust 检查 | `cargo check` | 在 `src-tauri/` 目录下运行 |
| Rust 测试 | `cargo test` | 在 `src-tauri/` 目录下运行 |

**lint/格式化**：项目没有配置 linter 或 formatter（无 eslint、prettier、rustfmt 配置）。不要运行不存在的东西。编辑代码时请遵循现有风格。

## CI / 发布

- `.github/workflows/build.yml` — 推送 `v*` tag 触发，在 Windows + macOS (Intel + ARM) 上并行构建
- 产物上传到 GitHub Release（draft），使用 `tauri-action`
- 触发：`git tag v0.1.0 && git push origin v0.1.0`

## 架构要点

### 前端 (`src/`)
- Vue 3 + TypeScript，4 个组件：`GatewayStatusView`(仪表盘)、`ProviderList`(提供商)、`ConfigEditor`(设置)、`ChatView`(聊天)
- `src/api/commands.ts` — 所有 Tauri IPC 命令封装 + 类型定义（与 Rust 结构体一一对应）
- 日志状态提升到 `App.vue` 层级，避免切换页面时组件卸载导致日志丢失

### 后端 (`src-tauri/src/`)
- `lib.rs` — Tauri 入口，注册了 **11 个 IPC 命令** 和 2 个事件（`gateway-log`、`gateway-state-changed`）
- `handler.rs` + `router.rs` — 嵌入的 Axum HTTP 网关
- `converter.rs` + `stream.rs` — Chat↔Responses↔Anthropic 协议转换（流式 + 非流式）
- `provider.rs` — 提供商运行时、密钥状态管理、故障转移逻辑
- `db.rs` — SQLite 持久化（bundled rusqlite）
- `config.rs` — `Config`、`Protocol`、`ApiKeyEntry` 结构体
- `log_bridge.rs` — 日志系统：tracing 事件桥接到 log4rs 文件日志 + 前端事件转发
- 网关不暴露独立端口；通过 Tauri 进程内运行 Axum

### 日志系统
- 文件日志用 **log4rs**：按天+按大小(10MB)双滚动，gzip 归档压缩，保留 30 天，总大小上限 10GB
- tracing 事件通过 `Log4rsBridgeLayer` 桥接到 log4rs（`log` 门面）
- 终端输出用 `tracing-subscriber` fmt 层（本地时区，ChronoLocal）
- 前端转发用自定义 `TauriLogLayer`（只转发 `ai_aggregs_lib` target 的日志）
- 日志级别通过 `EnvFilter` reload 实现运行时热更新

### 数据存储
- `data/config.db` — SQLite 数据库（在可执行文件旁边）
- `logs/` — 日志文件目录（在可执行文件旁边）

## 类型约定

- `ApiKeyEntry` 是 untagged 枚举，可以反序列化纯字符串或 `{ key, enabled }` 对象
- 前端 `normalizeKey()` 函数统一两种格式为 `{ key, enabled }`

## 测试注意事项

- **仅 Rust 端有测试基础设施**：`cargo test`（在 `src-tauri/` 目录下）
- **前端无测试**：没有配置测试运行器
- 不要在未先检查现有测试模式的情况下添加新测试

## 关键约定

- **关闭到系统托盘**：关闭窗口会隐藏到系统托盘，不会退出。通过托盘菜单退出。
- **`--minimized` 标志**：传递时窗口启动时隐藏（开机自启默认使用此标志）
- **协议自动检测**：通过请求 URL 路径：`/v1/chat/completions`（Chat）、`/v1/responses`（Responses）、`/v1/messages`（Anthropic）
- **故障转移**：按提供商顺序进行；仅在 429/5xx/超时时切换提供商。4xx（非 429）错误不会触发故障转移。
- **密钥 429 黑名单**：当所有密钥都被限流时，黑名单每 10 分钟全局重置一次
- **`consumer.models` 自动同步**：保存配置时，consumer models 会从所有已启用提供商的 models 中并集重新计算
