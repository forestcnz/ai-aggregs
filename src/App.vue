<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
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
let unlisten: (() => void) | null = null

async function refreshStatus() {
  try {
    status.value = await gatewayStatus()
  } catch (e) {
    console.error('gatewayStatus failed', e)
  }
}

onMounted(async () => {
  await refreshStatus()
  unlisten = await onGatewayStateChanged((running) => {
    status.value.running = running
    refreshStatus()
  })
})

onUnmounted(() => { unlisten?.() })
</script>

<template>
  <div class="app">
    <!-- 侧边栏 -->
    <nav class="sidebar">
      <div class="brand">
        <!-- Logo：三流汇聚线条版 -->
        <svg width="22" height="22" viewBox="0 0 64 64" fill="none">
          <path d="M5 19 C 21 19, 25 31, 33 32" stroke="currentColor" stroke-width="2.4" stroke-linecap="square"/>
          <path d="M5 32 L 33 32" stroke="var(--text-weak)" stroke-width="2.4" stroke-linecap="square"/>
          <path d="M5 45 C 21 45, 25 33, 33 32" stroke="currentColor" stroke-width="2.4" stroke-linecap="square"/>
          <path d="M33 32 L 59 32" stroke="currentColor" stroke-width="2.6" stroke-linecap="square"/>
          <rect x="29" y="28" width="8" height="8" rx="1" transform="rotate(45 33 32)" fill="none" stroke="currentColor" stroke-width="2.2"/>
        </svg>
        <span class="brand-name">ai<span class="dim">·</span>aggregs</span>
      </div>
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
        <code v-if="status.running">{{ status.listen_addr }}</code>
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
</template>

<style scoped>
.app {
  display: flex;
  height: 100vh;
  background: var(--bg);
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
.brand {
  display: flex; align-items: center; gap: 9px;
  padding: 4px 8px 18px;
  color: var(--text-strong);
}
.brand-name {
  font-weight: 600; font-size: 13px; letter-spacing: -.01em;
  color: var(--text-strong);
}
.brand-name .dim { color: var(--text-weak); }
.nav-item {
  display: flex; align-items: center; gap: 10px;
  padding: 8px 10px; border-radius: var(--r-sm);
  color: var(--text); font-size: 13px; font-weight: 400;
  text-decoration: none; transition: var(--transition);
}
.nav-item:hover { background: var(--bg-weak); color: var(--text-strong); }
/* 激活态：近黑底反白字 — opencode 风 */
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
.nav-footer code { font-size: 10px; color: var(--text); }
.nav-footer .dot {
  width: 6px; height: 6px; border-radius: 50%;
}
.nav-footer .dot.on { background: var(--green); }
.nav-footer .dot.off { background: var(--text-weaker); }
.content {
  flex: 1; overflow-y: auto; padding: 24px 28px;
  background: var(--bg);
}
</style>
