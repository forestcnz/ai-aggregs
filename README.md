<div align="center">

# ai·aggregs

**AI API 聚合网关桌面应用**

一个本地端点，反向代理多个上游提供商，支持 Chat / Responses / Anthropic 三种协议互转与自动故障转移。

Tauri v2 壳 + Vue 3 前端 + Rust/Axum 网关嵌入同一进程。

![Tauri v2](https://img.shields.io/badge/Tauri-v2-blue)
![Vue 3](https://img.shields.io/badge/Vue-3-42b883)
![Rust/Axum](https://img.shields.io/badge/Rust-Axum-dea584)
![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS-lightgrey)

</div>

---

## 截图预览

**网关状态**

![网关状态](preview/网关状态.png)

**供应商管理**

![供应商管理](preview/供应商管理.png)

**编辑/新增供应商**

![编辑新增供应商](preview/编辑新增供应商.png)

**AI 聊天**

![AI聊天](preview/AI聊天.png)

**用量统计**

![用量统计](preview/用量统计.png)

**供量统计**

![供量统计](preview/供量统计.png)

**设置**

![设置（一）](preview/设置1.png)

![设置（二）](preview/设置2.png)

**重设计稿**（复刻 opencode.ai 美学；[交互式版本](preview/redesign/index.html)，每个功能独立成页）：

![设计稿总览 — hero / logo / 设计系统 / 九页导航](preview/redesign/_overview.png)

<details>
<summary><b>九个功能页预览（点开查看）</b></summary>

**网关状态**

![网关状态](preview/redesign/dashboard.png)

**供应商**

![供应商](preview/redesign/providers.png)

**AI 聊天**

![AI聊天](preview/redesign/chat.png)

**用量统计**

![用量统计](preview/redesign/usage.png)

**供量统计**

![供量统计](preview/redesign/provider-usage.png)

**Codex 配置**

![Codex](preview/redesign/codex.png)

**OpenCode 配置**

![OpenCode](preview/redesign/opencode.png)

**Claude Code 配置**

![Claude Code](preview/redesign/claude-code.png)

**设置**

![设置](preview/redesign/settings.png)

</details>

---

## 功能特性

- **多提供商聚合** — 在一个本地端点后挂多个上游 API（OpenAI、Anthropic、GLM 等），对外暴露统一接口
- **三协议互转** — Chat / Responses / Anthropic 三种协议可任意转换（流式与非流式），按请求 URL 路径自动判定
- **自动故障转移** — 按 provider 顺序尝试；仅在 429 / 5xx / 超时时切换；密钥被限流后加入黑名单，每 10 分钟全局重置一次
- **密钥池管理** — 每个提供商支持多密钥轮换，单 key 级别启用/禁用与黑名单状态追踪
- **网关自动恢复** — 启动应用时按配置恢复上次网关运行状态，开机自启无感衔接
- **用量统计** — Token 级计量与持久化，区分 consumer 用量与 provider 供量双视图
- **系统托盘驻留** — 关闭窗口即隐藏到托盘，不退出进程；支持 `--minimized` 静默启动
- **SQLite 持久化** — 配置与用量存储于可执行文件旁的 `config.db`
- **日志体系** — log4rs 按天 + 大小双滚动，gzip 归档，保留 30 天 / 上限 10GB，可热更新级别并实时回传前端

## 协议自动检测

网关根据请求路径自动判定下游使用的协议，并按目标 provider 的协议进行转换：

| 请求路径 | 下游协议 |
|----------|----------|
| `/v1/chat/completions` | Chat |
| `/v1/responses` | Responses |
| `/v1/messages` | Anthropic |

> 例如：下游用 OpenAI Chat 格式请求，网关可自动转换为 Anthropic 协议转发给 Claude 提供商。

## 技术栈

| 层 | 技术 |
|----|------|
| 壳 | Tauri v2（同进程内嵌 Axum 网关，不暴露独立端口） |
| 前端 | Vue 3 `<script setup>` + TypeScript + Vite，features 模式组织 |
| 网关 | Rust + Axum（异步反向代理 + SSE 流式状态机） |
| 存储 | SQLite（bundled rusqlite） |
| 包管理 | JS → Bun，Rust → Cargo |

## 快速开始

### 环境要求

- [Node.js](https://nodejs.org/) + [Bun](https://bun.sh/)
- [Rust](https://www.rust-lang.org/) (stable)
- [Tauri v2 前置依赖](https://v2.tauri.app/start/prerequisites/)

### 开发

```bash
bun install          # 安装前端依赖
bun run tauri dev    # 完整应用（Vite 热重载 + Tauri 窗口）
# 或仅前端
bun run dev          # Vite 服务器 :1420，无 Tauri
```

### 验证

```bash
bun run lint         # 前端 ESLint
bun run build        # 类型检查（vue-tsc）+ Vite 构建
cd src-tauri && cargo check    # Rust 检查
cd src-tauri && cargo clippy   # Rust lint
cd src-tauri && cargo test     # Rust 测试
```

### 打包

```bash
bun run tauri build   # 生成 MSI / NSIS / DMG 安装包
```

## 项目结构

```
ai-aggregs/
├── src/                      # 前端（Vue 3）
│   ├── api/commands.ts       # Tauri IPC 封装 + 类型（与 Rust 一一对应）
│   ├── App.vue               # 根组件，日志状态提升
│   └── features/             # 按功能拆分
│       ├── dashboard/        # 网关状态（仪表盘）
│       ├── providers/        # 供应商管理
│       ├── chat/             # AI 聊天
│       ├── usage/            # consumer 用量统计
│       ├── provider-usage/   # 供应商用量统计
│       ├── opencode-config/  # opencode.jsonc 表单编辑
│       ├── claude-code-config/ # ~/.claude/settings.json env 编辑
│       ├── codex-config/     # ~/.codex/config.toml 编辑
│       └── settings/         # 设置
├── src-tauri/src/            # 后端（Rust/Axum）
│   ├── lib.rs                # Tauri 入口（日志/数据库/托盘/IPC 命令）
│   ├── api/                  # IPC 命令 + HTTP 请求处理 + 路由
│   ├── gateway/              # 网关生命周期 / provider / 协议转换 / 流式
│   ├── config/               # 配置类型与状态
│   └── infra/                # 数据库 / 错误 / 日志桥接 / 托盘
├── preview/                  # 界面截图 + 重设计稿
│   ├── *.png                 # 各页面截图
│   └── redesign/             # 交互式重设计稿（汇总页 + 每功能一页）
│       ├── index.html        # 汇总页（hero / logo / 设计系统 / 九页导航）
│       ├── styles.css        # 共享样式
│       ├── app.js            # 共享交互（导航 / 弹窗 / 拖拽 / 用量）
│       └── pages/            # 九个功能各一页（dashboard · providers · …）
└── data/config.db            # SQLite 持久化（运行时生成）
```

<div align="center">

Made with Tauri · Vue · Rust

</div>
