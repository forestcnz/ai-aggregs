<script setup lang="ts">
import { computed } from 'vue'
import { useApp } from './App'
import { provideDialog } from './composables/useDialog'
import AppToast from './components/AppToast.vue'
import AppConfirm from './components/AppConfirm.vue'
import GatewayStatusView from './features/dashboard/index.vue'
import ProviderList from './features/providers/index.vue'
import ConfigEditor from './features/settings/index.vue'
import ChatView from './features/chat/index.vue'
import UsageView from './features/usage/index.vue'
import ProviderUsageView from './features/provider-usage/index.vue'
import OpencodeConfigView from './features/opencode-config/index.vue'
import ClaudeCodeConfigView from './features/claude-code-config/index.vue'
import CodexConfigView from './features/codex-config/index.vue'

// 全局弹窗状态注入（在挂载子组件前完成 provide）
provideDialog()

const {
  activeTab,
  status,
  isMaximized,
  ocExists,
  ccExists,
  cdxExists,
  ready,
  logs,
  refreshStatus,
  minimize,
  toggleMaximize,
  closeWindow
} = useApp()

// 侧边栏导航：通过配置文件是否存在决定是否显示对应入口；
// 「设置」始终在列表最后一个
const navTabs = computed(() => {
  const tabs: { id: string; label: string }[] = [
    { id: 'dashboard', label: '网关状态' },
    { id: 'providers', label: '供应商' },
    { id: 'chat', label: 'AI聊天' },
    { id: 'usage', label: '用量统计' },
    { id: 'provider-usage', label: '供量统计' }
  ]
  if (cdxExists.value) tabs.push({ id: 'codex', label: 'Codex' })
  if (ocExists.value) tabs.push({ id: 'opencode', label: 'OpenCode' })
  if (ccExists.value) tabs.push({ id: 'claude-code', label: 'Claude Code' })
  tabs.push({ id: 'settings', label: '设置' })
  return tabs
})
</script>

<template>
  <div class="app">
    <!-- 全局弹窗（toast + 确认框） -->
    <AppToast />
    <AppConfirm />
    <!-- 自定义标题栏 — opencode.ai 风格 -->
    <header class="titlebar" data-tauri-drag-region>
      <!-- 左：品牌名 -->
      <div class="titlebar-left" data-tauri-drag-region>
        <span class="titlebar-brand" data-tauri-drag-region>AI 聚合网关</span>
      </div>

      <!-- 中：网关状态（仅运行时显示） -->
      <div v-if="status.running" class="titlebar-center" data-tauri-drag-region>
        <span class="titlebar-pill" data-tauri-drag-region>
          <span class="dot on"></span>
          <span class="pill-text">运行中</span>
          <span class="pill-addr">{{ status.listen_addr }}</span>
        </span>
      </div>

      <!-- 右：窗口控制 -->
      <div class="titlebar-controls">
        <button class="win-btn" title="最小化" aria-label="最小化" @click="minimize">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
            <line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" stroke-width="1" />
          </svg>
        </button>
        <button
          class="win-btn"
          :title="isMaximized ? '还原' : '最大化'"
          :aria-label="isMaximized ? '还原' : '最大化'"
          @click="toggleMaximize"
        >
          <!-- 最大化/还原图标 -->
          <svg v-if="!isMaximized" width="10" height="10" viewBox="0 0 10 10" fill="none">
            <rect
              x="1.5"
              y="1.5"
              width="7"
              height="7"
              fill="none"
              stroke="currentColor"
              stroke-width="1"
            />
          </svg>
          <svg v-else width="10" height="10" viewBox="0 0 10 10" fill="none">
            <rect
              x="2.5"
              y="0.5"
              width="6"
              height="6"
              fill="none"
              stroke="currentColor"
              stroke-width="1"
            />
            <rect
              x="0.5"
              y="2.5"
              width="6"
              height="6"
              fill="none"
              stroke="var(--bg)"
              stroke-width="1.5"
            />
            <rect
              x="0.5"
              y="2.5"
              width="6"
              height="6"
              fill="none"
              stroke="currentColor"
              stroke-width="1"
            />
          </svg>
        </button>
        <button class="win-btn win-close" title="关闭" aria-label="关闭" @click="closeWindow">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
            <line x1="1" y1="1" x2="9" y2="9" stroke="currentColor" stroke-width="1" />
            <line x1="9" y1="1" x2="1" y2="9" stroke="currentColor" stroke-width="1" />
          </svg>
        </button>
      </div>
    </header>

    <!-- 主体区 -->
    <div class="body">
      <!-- 未就绪：整页加载态，覆盖侧边栏 + 内容区，就绪后两者一起渲染 -->
      <div v-if="!ready" class="app-loading">
        <div class="spinner"></div>
        <div class="brand">AI 聚合网关</div>
        <div class="sub">正在初始化…</div>
      </div>
      <template v-else>
      <!-- 侧边栏 -->
      <nav class="sidebar">
        <a
          v-for="tab in navTabs"
          :key="tab.id"
          href="#"
          class="nav-item"
          :class="{ active: activeTab === tab.id }"
          @click.prevent="activeTab = tab.id as any"
        >
          <!-- 导航图标：线条方头 -->
          <svg v-if="tab.id === 'dashboard'" class="nav-icon" viewBox="0 0 14 14" fill="none">
            <rect
              x="1.5"
              y="1.5"
              width="4.5"
              height="4.5"
              rx="1"
              stroke="currentColor"
              stroke-width="1.4"
            />
            <rect
              x="8"
              y="1.5"
              width="4.5"
              height="4.5"
              rx="1"
              stroke="currentColor"
              stroke-width="1.4"
            />
            <rect
              x="1.5"
              y="8"
              width="4.5"
              height="4.5"
              rx="1"
              stroke="currentColor"
              stroke-width="1.4"
            />
            <rect
              x="8"
              y="8"
              width="4.5"
              height="4.5"
              rx="1"
              stroke="currentColor"
              stroke-width="1.4"
            />
          </svg>
          <svg v-else-if="tab.id === 'providers'" class="nav-icon" viewBox="0 0 14 14" fill="none">
            <path
              d="M2 3.5h10M2 7h10M2 10.5h6"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="square"
            />
            <rect
              x="10.5"
              y="9.5"
              width="3"
              height="3"
              rx="0.5"
              stroke="currentColor"
              stroke-width="1.3"
            />
          </svg>
          <svg v-else-if="tab.id === 'chat'" class="nav-icon" viewBox="0 0 14 14" fill="none">
            <path
              d="M2 2.5h10v6.5H6L4 11V9H2z"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linejoin="miter"
            />
            <path
              d="M4.5 5h5M4.5 7h3"
              stroke="currentColor"
              stroke-width="1.2"
              stroke-linecap="square"
            />
          </svg>
          <svg v-else-if="tab.id === 'usage'" class="nav-icon" viewBox="0 0 14 14" fill="none">
            <line
              x1="2.5"
              y1="11.5"
              x2="2.5"
              y2="8.5"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="square"
            />
            <line
              x1="6"
              y1="11.5"
              x2="6"
              y2="4.5"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="square"
            />
            <line
              x1="9.5"
              y1="11.5"
              x2="9.5"
              y2="6.5"
              stroke="currentColor"
              stroke-width="1.8"
              stroke-linecap="square"
            />
            <line
              x1="1.5"
              y1="12.5"
              x2="12.5"
              y2="12.5"
              stroke="currentColor"
              stroke-width="1.2"
              stroke-linecap="square"
            />
          </svg>
          <svg
            v-else-if="tab.id === 'provider-usage'"
            class="nav-icon"
            viewBox="0 0 14 14"
            fill="none"
          >
            <rect
              x="1.5"
              y="1.5"
              width="4.5"
              height="4.5"
              rx="1"
              stroke="currentColor"
              stroke-width="1.3"
            />
            <line
              x1="8"
              y1="3.5"
              x2="12.5"
              y2="3.5"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="square"
            />
            <line
              x1="3.5"
              y1="8"
              x2="3.5"
              y2="12.5"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linecap="square"
            />
            <rect
              x="7"
              y="7"
              width="5.5"
              height="5.5"
              rx="1"
              stroke="currentColor"
              stroke-width="1.3"
            />
          </svg>
          <svg
            v-else-if="tab.id === 'opencode'"
            class="nav-icon"
            viewBox="0 0 512 512"
            fill="none"
          >
            <!-- OpenCode 官方 logo：方形外框（带孔）+ 孔下半部方块 -->
            <path
              fill-rule="evenodd"
              clip-rule="evenodd"
              d="M384 416H128V96H384V416ZM320 160H192V352H320V160Z"
              fill="currentColor"
            />
            <path d="M320 352V224H192V352H320Z" fill="currentColor" opacity="0.45" />
          </svg>
          <svg
            v-else-if="tab.id === 'claude-code'"
            class="nav-icon"
            viewBox="0 0 24 24"
            fill="currentColor"
          >
            <!-- Claude 官方 sunburst 图标（simple-icons） -->
            <path
              d="m4.714 15.956l4.718-2.648l.079-.23l-.08-.128h-.23l-.79-.048l-2.695-.073l-2.337-.097l-2.265-.122l-.57-.121l-.535-.704l.055-.353l.48-.321l.685.06l1.518.104l2.277.157l1.651.098l2.447.255h.389l.054-.158l-.133-.097l-.103-.098l-2.356-1.596l-2.55-1.688l-1.336-.972l-.722-.491L2 6.223l-.158-1.008l.656-.722l.88.06l.224.061l.893.686l1.906 1.476l2.49 1.833l.364.304l.146-.104l.018-.072l-.164-.274l-1.354-2.446l-1.445-2.49l-.644-1.032l-.17-.619a3 3 0 0 1-.103-.729L6.287.133L6.7 0l.995.134l.42.364l.619 1.415L9.735 4.14l1.555 3.03l.455.898l.243.832l.09.255h.159V9.01l.127-1.706l.237-2.095l.23-2.695l.08-.76l.376-.91l.747-.492l.583.28l.48.685l-.067.444l-.286 1.851l-.558 2.903l-.365 1.942h.213l.243-.242l.983-1.306l1.652-2.064l.728-.82l.85-.904l.547-.431h1.032l.759 1.129l-.34 1.166l-1.063 1.347l-.88 1.142l-1.263 1.7l-.79 1.36l.074.11l.188-.02l2.853-.606l1.542-.28l1.84-.315l.832.388l.09.395l-.327.807l-1.967.486l-2.307.462l-3.436.813l-.043.03l.049.061l1.548.146l.662.036h1.62l3.018.225l.79.522l.473.638l-.08.485l-1.213.62l-1.64-.389l-3.825-.91l-1.31-.329h-.183v.11l1.093 1.068l2.003 1.81l2.508 2.33l.127.578l-.321.455l-.34-.049l-2.204-1.657l-.85-.747l-1.925-1.62h-.127v.17l.443.649l2.343 3.521l.122 1.08l-.17.353l-.607.213l-.668-.122l-1.372-1.924l-1.415-2.168l-1.141-1.943l-.14.08l-.674 7.254l-.316.37l-.728.28l-.607-.461l-.322-.747l.322-1.476l.388-1.924l.316-1.53l.285-1.9l.17-.632l-.012-.042l-.14.018l-1.432 1.967l-2.18 2.945l-1.724 1.845l-.413.164l-.716-.37l.066-.662l.401-.589l2.386-3.036l1.439-1.882l.929-1.086l-.006-.158h-.055L4.138 18.56l-1.13.146l-.485-.456l.06-.746l.231-.243l1.907-1.312Z"
            />
          </svg>
          <svg v-else-if="tab.id === 'codex'" class="nav-icon" viewBox="0 0 24 24" fill="currentColor">
            <!-- Codex 官方 logo（lobe-icons mono）：云形主体 + 终端下划线 -->
            <path
              fill-rule="evenodd"
              clip-rule="evenodd"
              d="M8.086.457a6.105 6.105 0 013.046-.415c1.333.153 2.521.72 3.564 1.7a.117.117 0 00.107.029c1.408-.346 2.762-.224 4.061.366l.063.03.154.076c1.357.703 2.33 1.77 2.918 3.198.278.679.418 1.388.421 2.126a5.655 5.655 0 01-.18 1.631.167.167 0 00.04.155 5.982 5.982 0 011.578 2.891c.385 1.901-.01 3.615-1.183 5.14l-.182.22a6.063 6.063 0 01-2.934 1.851.162.162 0 00-.108.102c-.255.736-.511 1.364-.987 1.992-1.199 1.582-2.962 2.462-4.948 2.451-1.583-.008-2.986-.587-4.21-1.736a.145.145 0 00-.14-.032c-.518.167-1.04.191-1.604.185a5.924 5.924 0 01-2.595-.622 6.058 6.058 0 01-2.146-1.781c-.203-.269-.404-.522-.551-.821a7.74 7.74 0 01-.495-1.283 6.11 6.11 0 01-.017-3.064.166.166 0 00.008-.074.115.115 0 00-.037-.064 5.958 5.958 0 01-1.38-2.202 5.196 5.196 0 01-.333-1.589 6.915 6.915 0 01.188-2.132c.45-1.484 1.309-2.648 2.577-3.493.282-.188.55-.334.802-.438.286-.12.573-.22.861-.304a.129.129 0 00.087-.087A6.016 6.016 0 015.635 2.31C6.315 1.464 7.132.846 8.086.457zm-.804 7.85a.848.848 0 00-1.473.842l1.694 2.965-1.688 2.848a.849.849 0 001.46.864l1.94-3.272a.849.849 0 00.007-.854l-1.94-3.393zm5.446 6.24a.849.849 0 000 1.695h4.848a.849.849 0 000-1.696h-4.848z"
            />
          </svg>
          <svg v-else class="nav-icon" viewBox="0 0 14 14" fill="none">
            <circle cx="7" cy="7" r="2" stroke="currentColor" stroke-width="1.3" />
            <path
              d="M7 1.5v1.7M7 10.8v1.7M12.5 7h-1.7M3.2 7H1.5M10.9 3.1l-1.2 1.2M4.3 9.7l-1.2 1.2M10.9 10.9l-1.2-1.2M4.3 4.3L3.1 3.1"
              stroke="currentColor"
              stroke-width="1.2"
              stroke-linecap="square"
            />
          </svg>
          {{ tab.label }}
        </a>
        <div class="nav-spacer" />
        <div class="nav-footer">
          <span class="dot" :class="status.running ? 'on' : 'off'"></span>
          {{ status.running ? '运行中' : '已停止' }}
        </div>
      </nav>

      <!-- 内容区 — 仅缓存 ChatView（保留聊天记录/输入/发送状态），
           其它 tab 切换时仍销毁重建以确保最新状态 -->
      <main class="content" :class="{ 'content-flush': activeTab === 'dashboard' }">
        <KeepAlive include="ChatView">
          <GatewayStatusView
            v-if="activeTab === 'dashboard'"
            :status="status"
            :logs="logs"
            @changed="refreshStatus"
            @clear-logs="logs = []"
          />
          <ProviderList v-else-if="activeTab === 'providers'" :gateway-running="status.running" />
          <ChatView v-else-if="activeTab === 'chat'" :status="status" />
          <UsageView v-else-if="activeTab === 'usage'" :status="status" />
          <ProviderUsageView v-else-if="activeTab === 'provider-usage'" :status="status" />
          <ConfigEditor v-else-if="activeTab === 'settings'" />
          <OpencodeConfigView v-else-if="activeTab === 'opencode'" :version="null" />
          <ClaudeCodeConfigView
            v-else-if="activeTab === 'claude-code'"
            :version="null"
          />
          <CodexConfigView v-else-if="activeTab === 'codex'" :version="null" />
        </KeepAlive>
      </main>
      </template>
    </div>
  </div>
</template>

<style src="./App.css" scoped></style>
