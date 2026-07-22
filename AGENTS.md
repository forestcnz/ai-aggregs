# ai-aggregs — Agent 指南

Tauri v2 桌面应用：Vue 3 前端 + Rust/Axum 网关运行在**同一进程**（网关并非独立服务，Axum 监听器挂在 Tauri 应用内，对外仅暴露用户配置的 `listen` 地址）。前端 ↔ 后端通过 Tauri IPC `invoke()` 通信，没有 REST 边界。

## 禁止执行（耗时长 / 破坏缓存 / 启动 GUI）

| 命令 | 原因 |
|------|------|
| `cargo clean` | 删除构建缓存，后续编译极慢 |
| `bun run tauri dev` | 启动完整 Tauri 窗口（GUI），阻塞终端 |
| `bun run tauri build` | 打包完整安装包，耗时极长 |

## 验证改动

| 目的 | 命令 | 备注 |
|------|------|------|
| 前端 lint | `bun run lint` | 仅检查 `src/` |
| 前端类型检查 + 构建 | `bun run build` | **这就是类型检查**：脚本先跑 `vue-tsc --noEmit` 再 `vite build`。无独立 `typecheck` 脚本 |
| Rust 检查 / lint / 测试 | `cargo check` / `cargo clippy` / `cargo test` | **workdir = `src-tauri`** |
| 格式化前端 | `bun run format` | prettier；或 `format:check` 只校验 |

前端**没有测试运行器**（无 vitest/jest），前端验证 = `lint` + `build`。唯一的测试套件是 Rust 单元/集成测试，集中在协议转换：`src-tauri/src/gateway/tests.rs`。

## 架构关键点（文件名看不出来的）

- **网关进程模型**：Axum `Router`（`src-tauri/src/api/router.rs`）在 Tauri 应用进程内启动，按请求 URL 路径判定下游协议并转换：
  - `/v1/chat/completions` → Chat，`/v1/responses` → Responses，`/v1/messages` → Anthropic
  - 三协议经统一 IR（`gateway/ir/`）互转，单跳完成（`gateway/converter.rs::req_convert`）。改转换逻辑务必同步 `gateway/tests.rs` 的 round-trip / regression 用例。
- **持久化位置**：SQLite（`rusqlite` 带 `bundled` 特性，随二进制编译）。数据库与日志**写在可执行文件同级目录**，不是工作目录：
  - `data/config.db`（`infra/db.rs`），`logs/`（log4rs，按天+10MB 双滚动，gzip，保留 30 天 / 上限 10GB）
  - 调试时去 `target/...` 或安装目录下找，别在仓库根目录找。

## 新增 / 修改 IPC 命令（最容易漏步骤）

一次完整的 IPC 契约改动需要**三处协同**，缺一会静默失败或类型错位：

1. **Rust**：`src-tauri/src/api/commands.rs` 写 `#[tauri::command]` 函数
2. **Rust**：`src-tauri/src/lib.rs` 的 `invoke_handler![...]` 列表里注册该函数名
3. **TS**：`src/api/commands.ts` 加 `invoke('xxx')` 封装，**类型必须与 Rust 结构体一一对应**（该文件头注明了这一点）。结构体定义在 `src-tauri/src/config/types.rs`、`config/state.rs`。

窗口为 `decorations: false`（自定义标题栏），前端自行处理最小化/最大化/关闭/拖拽；新增窗口控制需在 `capabilities/default.json` 补对应 `core:window:*` 权限。

## 代码风格（偏离常见默认值）

- Prettier：**无分号**、单引号、`printWidth: 100`、**无尾随逗号**（`trailingComma: "none"`）
- ESLint：`@typescript-eslint/no-explicit-any` 已关闭（`any` 允许用）；`vue/multi-word-component-names` 关闭
- 前端用 Vue 3 `<script setup>` + TS，按 `src/features/<功能>/` 切分；Rust edition 2021，`rustfmt` `max_width = 100`
- JS 用 **Bun**（`bun install` / `bun run ...`），不用 npm/yarn

## 发版

CI（`.github/workflows/build.yml`）仅在推送 `v*` tag 时触发，矩阵构建 Windows + macOS（aarch64 / x86_64），用 `tauri-apps/tauri-action` 发布 GitHub Release。本地不要跑 `tauri build`。
