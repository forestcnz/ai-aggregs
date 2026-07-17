# ai-aggregs — Agent 指南

AI API 聚合网关桌面应用。Tauri v2 + Vue 3 + Rust/Axum，同一进程内嵌网关。

## 包管理器

- **JS → Bun**，勿用 npm/yarn/pnpm
- **Rust → Cargo** (`src-tauri/`)

## 禁止执行

AI 不得执行以下命令（耗时长、会破坏缓存或启动 GUI 进程，不适用于自动化工作流）：

| 命令 | 原因 |
|------|------|
| `cargo clean` | 删除构建缓存，导致后续编译极慢 |
| `bun run tauri dev` | 启动完整 Tauri 窗口（GUI），阻塞终端 |
| `bun run tauri build` | 打包完整安装包，耗时极长 |

如需验证改动，使用 `cargo check`、`cargo clippy`、`bun run build` 等轻量命令替代。

## 关键命令

| 用途 | 命令 | 说明 |
|------|------|------|
| 完整应用开发 | `bun run tauri dev` | Vite 热重载 + Tauri 窗口 |
| 仅前端开发 | `bun run dev` | Vite 服务器 :1420，无 Tauri |
| 类型检查+构建 | `bun run build` | `vue-tsc --noEmit && vite build` |
| Tauri 安装包 | `bun run tauri build` | MSI/NSIS/DMG |
| 前端 lint | `bun run lint` | ESLint `src/` |
| 前端 lint 修复 | `bun run lint:fix` | |
| 前端格式化 | `bun run format` | Prettier `src/**/*.{ts,vue,css}` |
| Rust check | `cargo check` | 在 `src-tauri/` 下运行 |
| Rust lint | `cargo clippy` | 在 `src-tauri/` 下运行 |
| Rust 格式化 | `cargo fmt` | 在 `src-tauri/` 下运行 |
| Rust 测试 | `cargo test` | 在 `src-tauri/` 下运行 |

lint/format 顺序：编辑 → `bun run lint:fix` → `cargo clippy`(Rust) → `bun run build`(类型检查)。

## 前端 (`src/`)

### 文件结构 (features 模式)

```
src/
├── api/commands.ts       ← Tauri IPC 封装 + 类型（与 Rust 一一对应）+ 共享工具（maskKey/normalizeKey）
├── App.{vue,ts,css}      ← 根组件，日志状态提升到此层级；provideDialog() 注入点
├── components/           ← 全局通用组件
│   ├── AppToast.vue      ← 全局 toast 容器（由 useDialog 驱动）
│   ├── AppModal.vue      ← 通用遮罩 + 卡片壳（slot-based）
│   └── AppConfirm.vue    ← 全局确认/提示框（由 useDialog 驱动）
├── composables/
│   └── useDialog.ts      ← 全局弹窗状态 + API（toast/alert/confirm）
├── features/             ← 按功能拆分
│   ├── dashboard/        ← 网关状态（仪表盘）
│   ├── providers/        ← 提供商管理
│   ├── chat/             ← 聊天
│   ├── usage/            ← consumer 用量统计
│   ├── provider-usage/   ← 供应商用量统计
│   ├── opencode-config/  ← opencode.json 表单编辑
│   ├── claude-code-config/ ← ~/.claude/settings.json 的 env 段编辑
│   └── settings/         ← 设置
```

每个 feature 含 `index.vue`(template + props/emits)、`index.ts`(composable 逻辑)、`index.css`(样式)。

### 模式

- **Composable 提取**：逻辑在 `index.ts` 中导出 `useXxx()` 函数，`.vue` 中调用并解构
- **CSS 提取**：`<style src="./index.css" scoped>`
- **Props/Emits**：保留在 `.vue` 中（编译器宏，不可外移）
- **ESLint / vue-tsc**：外部 `.ts` 文件中的 bindings 通过 composable 返回值连接模板，no-unused-vars 在 eslint 配置中放行（`no-explicit-any: off`, `vue/multi-word-component-names: off`）
- **Prettier**：`semi: false`, `singleQuote: true`, `printWidth: 100`, `trailingComma: "none"`, `tabWidth: 2`

### 日志

日志状态提升到 `App.vue`，避免切页丢失。通过 `gateway-log` 事件接收，最多保留 500 条。

### 弹窗（toast / alert / confirm）

**统一通过 `useDialog()` 调用，禁止在 feature 内自建 `msg` ref / `dialogMsg` / 本地 overlay。** `App.vue` 启动时 `provideDialog()` 注入全局状态，`<AppToast/>` + `<AppConfirm/>` 挂在根节点。

```ts
const { toast, alert, confirm } = useDialog()
toast('保存成功', 'success')          // info | success | error，默认 2400ms 自动消失
await alert({ title: '失败', message: String(e) })  // 单按钮，返回 Promise<void>
const ok = await confirm({ message: '删除？', danger: true, confirmText: '删除' })  // 返回 boolean
```

- provider 拖拽排序后用 `save(true)` 静默保存（成功不提示，失败才提示）
- feature 内需要模态框（如 providers 编辑表单）用 `<AppModal>`，通过 `open` prop + `@close` 控制，内容走 slot

## 后端 (`src-tauri/src/`)

### 模块布局

- `lib.rs` — Tauri 入口，初始化日志/数据库/托盘，注册 20 个 IPC 命令 + 2 个事件
- `api/commands.rs` — 所有 `#[tauri::command]` 函数
- `api/handler.rs` — Axum HTTP 请求处理（鉴权、model 路由、协议判定、failover）
- `api/router.rs` — Axum 路由表 + CORS
- `gateway/manager.rs` — 网关生命周期（启动/停止/重建/consumer models 同步）
- `gateway/provider.rs` — 提供商运行时、密钥状态、failover 逻辑
- `gateway/converter.rs` — Chat↔Responses↔Anthropic 非流式协议转换
- `gateway/stream.rs` — 流式协议转换（SSE 状态机）
- `config/types.rs` — `Config`、`Protocol`、`ApiKeyEntry` 等纯数据类型
- `config/state.rs` — `AppCtrl`、`AppState`、`ServerHandle`、IPC 返回类型
- `infra/db.rs` — SQLite 持久化（bundled rusqlite）
- `infra/error.rs` — `AppError`(Axum HTTP) + `IpcError`(Tauri IPC)
- `infra/log_bridge.rs` — tracing → log4rs 桥接 + 日志热更新 + 前端事件
- `infra/opencode.rs` — `opencode.json` 读写/解析/合并（剥注释、表单↔JSON 双向转换）
- `infra/claude_code.rs` — `~/.claude/settings.json` 的 `env` 段读写/合并（整体替换 env，保留其它顶层字段；备份 `.bak`；执行 `claude --version`）
- `infra/tray.rs` — 系统托盘

### 关键行为

- 网关不暴露独立端口，Axum 嵌在 Tauri 进程内
- 关闭窗口→隐藏到系统托盘，不从进程退出（托盘菜单退出）
- `--minimized` 标志：启动时隐藏窗口（开机自启用此标志）
- **async 命令中的 DB 操作必须用 `tauri::async_runtime::spawn_blocking` 包裹**（`save_config`、`UsageCtx::record` 已遵循此模式）。DB 连接是 `Arc<Mutex<rusqlite::Connection>>`（同步 rusqlite，非 async），在 async 路径直接 `.lock()` 会阻塞 tokio worker
- 流式请求（SSE）不应用总超时：`Provider::client` 只设 `connect_timeout`，非流式请求通过 `RequestBuilder::timeout()` 单独设置
- consumer key 比较用 `constant_time_eq`（防 timing attack）；未配置 key 时放行但启动告警

### 协议自动检测 (URL 路径)

| 路径 | 协议 |
|------|------|
| `/v1/chat/completions` | Chat |
| `/v1/responses` | Responses |
| `/v1/messages` | Anthropic |

### 故障转移规则

- 按配置的提供商顺序尝试
- 仅在 **429 / 5xx / 超时** 时切换提供商
- 4xx（非 429）不触发 failover
- 所有密钥都被限流时，黑名单每 10 分钟全局重置一次

### 类型约定

`ApiKeyEntry` 是 untagged 枚举：纯字符串 `"sk-xxx"` 或对象 `{ key, enabled }`。前端用 `normalizeKey()` 统一。

### 配置自动同步

保存配置时 `consumer.models` 从所有已启用提供商的 models 并集重新计算。

## 数据存储

- `data/config.db` — SQLite（在可执行文件旁）
- `logs/` — 日志文件（在可执行文件旁），log4rs 按天+大小双滚动，gzip 归档，保留 30 天，上限 10GB

## 测试

- **仅 Rust 端**有测试基础设施：`cargo test`（`src-tauri/` 下）
- **前端无测试运行器**，勿添加

## CI

`.github/workflows/build.yml` — 推送 `v*` tag 触发，在 Windows + macOS (Intel + ARM) 上并行构建。产物上传到 GitHub Release（非 draft），使用 `tauri-action`。
- 触发：`git tag v0.1.0 && git push origin v0.1.0`
- 手动触发：GitHub 页面 → Actions → Build → Run workflow
