<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  gatewayStatus, onGatewayStateChanged,
  type GatewayStatus,
} from './api/commands'
import GatewayStatusView from './components/GatewayStatusView.vue'
import ProviderList from './components/ProviderList.vue'
import ConfigEditor from './components/ConfigEditor.vue'
import ChatView from './components/ChatView.vue'

const activeTab = ref<'dashboard' | 'providers' | 'chat' | 'settings'>('dashboard')
const status = ref<GatewayStatus>({ running: false, listen_addr: '' })
const isMaximized = ref(false)
let unlistenStatus: (() => void) | null = null
let unlistenResize: (() => void) | null = null

const appWindow = getCurrentWindow()

async function refreshStatus() {
  try {
    status.value = await gatewayStatus()
  } catch (e) {
    console.error('gatewayStatus failed', e)
  }
}

async function checkMaximized() {
  try {
    isMaximized.value = await appWindow.isMaximized()
  } catch { /* ignore */ }
}

// 窗口控制
async function minimize() { await appWindow.minimize() }
async function toggleMaximize() { await appWindow.toggleMaximize() }
async function closeWindow() { await appWindow.close() } // 触发 close_requested → 隐藏到托盘

onMounted(async () => {
  await refreshStatus()
  await checkMaximized()
  unlistenStatus = await onGatewayStateChanged((running) => {
    status.value.running = running
    refreshStatus()
  })
  unlistenResize = await appWindow.onResized(() => checkMaximized())
})

onUnmounted(() => {
  unlistenStatus?.()
  unlistenResize?.()
})
</script>

<template>
  <div class="app">
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
        <button class="win-btn" @click="minimize" title="最小化" aria-label="最小化">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
            <line x1="1" y1="5" x2="9" y2="5" stroke="currentColor" stroke-width="1"/>
          </svg>
        </button>
        <button class="win-btn" @click="toggleMaximize" :title="isMaximized ? '还原' : '最大化'" :aria-label="isMaximized ? '还原' : '最大化'">
          <!-- 最大化/还原图标 -->
          <svg v-if="!isMaximized" width="10" height="10" viewBox="0 0 10 10" fill="none">
            <rect x="1.5" y="1.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1"/>
          </svg>
          <svg v-else width="10" height="10" viewBox="0 0 10 10" fill="none">
            <rect x="2.5" y="0.5" width="6" height="6" fill="none" stroke="currentColor" stroke-width="1"/>
            <rect x="0.5" y="2.5" width="6" height="6" fill="none" stroke="var(--bg)" stroke-width="1.5"/>
            <rect x="0.5" y="2.5" width="6" height="6" fill="none" stroke="currentColor" stroke-width="1"/>
          </svg>
        </button>
        <button class="win-btn win-close" @click="closeWindow" title="关闭" aria-label="关闭">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
            <line x1="1" y1="1" x2="9" y2="9" stroke="currentColor" stroke-width="1"/>
            <line x1="9" y1="1" x2="1" y2="9" stroke="currentColor" stroke-width="1"/>
          </svg>
        </button>
      </div>
    </header>

    <!-- 主体区 -->
    <div class="body">
      <!-- 侧边栏 -->
      <nav class="sidebar">
        <a
          v-for="tab in [
            { id: 'dashboard', label: '网关状态' },
            { id: 'providers', label: '提供商' },
            { id: 'chat', label: '聊天' },
            { id: 'settings', label: '设置' },
          ]"
          :key="tab.id"
          href="#"
          class="nav-item"
          :class="{ active: activeTab === tab.id }"
          @click.prevent="activeTab = tab.id as any"
        >
          <!-- 导航图标：线条方头 -->
          <svg v-if="tab.id === 'dashboard'" class="nav-icon" viewBox="0 0 14 14" fill="none">
            <rect x="1.5" y="1.5" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.4"/>
            <rect x="8" y="1.5" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.4"/>
            <rect x="1.5" y="8" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.4"/>
            <rect x="8" y="8" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.4"/>
          </svg>
          <svg v-else-if="tab.id === 'providers'" class="nav-icon" viewBox="0 0 14 14" fill="none">
            <path d="M2 3.5h10M2 7h10M2 10.5h6" stroke="currentColor" stroke-width="1.4" stroke-linecap="square"/>
            <rect x="10.5" y="9.5" width="3" height="3" rx="0.5" stroke="currentColor" stroke-width="1.3"/>
          </svg>
          <svg v-else-if="tab.id === 'chat'" class="nav-icon" viewBox="0 0 14 14" fill="none">
            <path d="M2 2.5h10v6.5H6L4 11V9H2z" stroke="currentColor" stroke-width="1.3" stroke-linejoin="miter"/>
            <path d="M4.5 5h5M4.5 7h3" stroke="currentColor" stroke-width="1.2" stroke-linecap="square"/>
          </svg>
          <svg v-else class="nav-icon" viewBox="0 0 14 14" fill="none">
            <circle cx="7" cy="7" r="2" stroke="currentColor" stroke-width="1.3"/>
            <path d="M7 1.5v1.7M7 10.8v1.7M12.5 7h-1.7M3.2 7H1.5M10.9 3.1l-1.2 1.2M4.3 9.7l-1.2 1.2M10.9 10.9l-1.2-1.2M4.3 4.3L3.1 3.1" stroke="currentColor" stroke-width="1.2" stroke-linecap="square"/>
          </svg>
          {{ tab.label }}
        </a>
        <div class="nav-spacer" />
        <div class="nav-footer">
          <span class="dot" :class="status.running ? 'on' : 'off'"></span>
          {{ status.running ? '运行中' : '已停止' }}
        </div>
      </nav>

      <!-- 内容区 -->
      <main class="content">
        <GatewayStatusView v-if="activeTab === 'dashboard'" :status="status" @changed="refreshStatus" />
        <ProviderList v-else-if="activeTab === 'providers'" :gateway-running="status.running" />
        <ChatView v-else-if="activeTab === 'chat'" :status="status" />
        <ConfigEditor v-else />
      </main>
    </div>
  </div>
</template>

<style scoped>
.app {
  display: flex;
  flex-direction: column;
  height: 100vh;
  background: var(--bg);
}

/* ============================================================
   自定义标题栏 — opencode.ai 风格
   ============================================================ */
.titlebar {
  height: 36px;
  flex-shrink: 0;
  display: flex;
  align-items: center;
  justify-content: space-between;
  border-bottom: 1px solid var(--border-weak);
  background: var(--bg);
  user-select: none;
  -webkit-app-region: drag;
}
.titlebar-left {
  display: flex; align-items: center; gap: 8px;
  padding-left: 12px;
}
.titlebar-logo { flex-shrink: 0; }
.titlebar-brand {
  font-weight: 600; font-size: 12px; color: var(--text-strong);
  letter-spacing: -.01em;
}
.titlebar-brand .dim { color: var(--text-weak); }

.titlebar-center {
  position: absolute; left: 50%; transform: translateX(-50%);
  display: flex; align-items: center;
}
.titlebar-pill {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 2px 8px; border: 1px solid var(--border-weak);
  border-radius: var(--r-sm); background: var(--bg);
  font-size: 10px; color: var(--text-weak);
}
.titlebar-pill .dot { width: 5px; height: 5px; border-radius: 50%; }
.titlebar-pill .dot.on { background: var(--green); }
.titlebar-pill .dot.off { background: var(--text-weaker); }
.titlebar-pill .pill-text { color: var(--text); }
.titlebar-pill .pill-addr { color: var(--text-weak); }

/* 窗口控制按钮 */
.titlebar-controls {
  display: flex; align-items: center;
  -webkit-app-region: no-drag;
}
.win-btn {
  width: 36px; height: 36px;
  display: flex; align-items: center; justify-content: center;
  background: transparent; border: none; cursor: pointer;
  color: var(--text-weak); transition: var(--transition);
  font-family: inherit;
}
.win-btn:hover { background: var(--bg-weak); color: var(--text-strong); }
.win-close:hover { background: var(--red); color: var(--text-inverted); }

/* ============================================================
   主体区
   ============================================================ */
.body {
  display: flex;
  flex: 1;
  overflow: hidden;
}
.sidebar {
  width: 200px;
  border-right: 1px solid var(--border-weak);
  background: var(--bg);
  display: flex;
  flex-direction: column;
  padding: 14px 12px;
  gap: 2px;
  flex-shrink: 0;
}
.nav-item {
  display: flex; align-items: center; gap: 10px;
  padding: 8px 10px; border-radius: var(--r-sm);
  color: var(--text); font-size: 13px; font-weight: 400;
  text-decoration: none; transition: var(--transition);
}
.nav-item:hover { background: var(--bg-weak); color: var(--text-strong); }
.nav-item.active {
  background: var(--bg-strong); color: var(--text-inverted);
}
.nav-icon { font-size: 14px; width: 14px; height: 14px; flex-shrink: 0; }
.nav-spacer { flex: 1; }
.nav-footer {
  display: flex; align-items: center; gap: 8px;
  padding: 10px 10px; font-size: 11px; color: var(--text-weak);
  border-top: 1px solid var(--border-weak); margin: 0 -12px -14px;
}
.nav-footer .dot { width: 6px; height: 6px; border-radius: 50%; }
.nav-footer .dot.on { background: var(--green); }
.nav-footer .dot.off { background: var(--text-weaker); }
.content {
  flex: 1; overflow-y: auto; padding: 24px 28px;
  background: var(--bg);
}
</style>
