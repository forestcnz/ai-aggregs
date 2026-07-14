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
| 前端 lint | `bun run lint` | ESLint 检查 `src/` |
| 前端格式化 | `bun run format` | Prettier 格式化 `src/**/*.{ts,vue,css}` |
| Rust 检查 | `cargo check` | 在 `src-tauri/` 目录下运行 |
| Rust lint | `cargo clippy` | 在 `src-tauri/` 目录下运行 |
| Rust 格式化 | `cargo fmt` | 在 `src-tauri/` 目录下运行 |
| Rust 测试 | `cargo test` | 在 `src-tauri/` 目录下运行 |

**lint/格式化**：项目已配置 ESLint + Prettier（前端）和 rustfmt + clippy（Rust）。编辑代码后请运行对应的 lint/format 命令。

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
- `lib.rs` — Tauri 入口，初始化日志/数据库/托盘，注册 **11 个 IPC 命令** 和 2 个事件
- `api/` — 对外接口层
  - `commands.rs` — 所有 `#[tauri::command]` 函数（配置管理、网关控制、运行时状态、自启）
  - `handler.rs` — Axum HTTP 请求处理（鉴权、model 路由、协议判定、failover）
  - `router.rs` — Axum 路由表（注册端点 + CORS 中间件）
- `gateway/` — 网关核心
  - `manager.rs` — 网关生命周期管理（启动、停止、重建、consumer models 同步）
  - `provider.rs` — 提供商运行时、密钥状态管理、故障转移逻辑
  - `converter.rs` — Chat↔Responses↔Anthropic 协议转换（请求体 + 非流式响应体）
  - `stream.rs` — Chat↔Responses↔Anthropic 流式协议转换（SSE 状态机）
- `config/` — 配置与运行时状态
  - `types.rs` — `Config`、`Protocol`、`ApiKeyEntry` 等纯数据类型的定义
  - `state.rs` — `AppCtrl`、`AppState`、`ServerHandle`、`TrayItems`、IPC 返回类型等运行时状态
- `infra/` — 基础设施
  - `db.rs` — SQLite 持久化（bundled rusqlite）
  - `error.rs` — 错误类型：`AppError`（Axum HTTP）+ `IpcError`（Tauri IPC）
  - `log_bridge.rs` — 日志系统：tracing 桥接到 log4rs 文件日志 + 前端事件转发
  - `tray.rs` — 系统托盘菜单构建和事件处理
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
