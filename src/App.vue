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
      <div class="brand">AI 聚合网关</div>
      <a
        v-for="tab in [
          { id: 'dashboard', label: '网关状态', icon: '▣' },
          { id: 'providers', label: '提供商', icon: '▤' },
          { id: 'chat', label: '聊天', icon: '✦' },
          { id: 'settings', label: '设置', icon: '⚙' },
        ]"
        :key="tab.id"
        href="#"
        class="nav-item"
        :class="{ active: activeTab === tab.id }"
        @click.prevent="activeTab = tab.id as any"
      >
        <span class="nav-icon">{{ tab.icon }}</span>
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
</template>

<style scoped>
.app {
  display: flex;
  height: 100vh;
  background: var(--bg-deep);
}
.sidebar {
  width: 200px;
  border-right: 1px solid var(--border);
  background: var(--bg-surface);
  display: flex;
  flex-direction: column;
  padding: 16px 12px;
  gap: 4px;
  flex-shrink: 0;
}
.brand {
  font-weight: 800; font-size: 18px; letter-spacing: -.5px;
  padding: 8px 14px 16px; color: var(--amber);
}
.nav-item {
  display: flex; align-items: center; gap: 10px;
  padding: 10px 14px; border-radius: var(--radius-sm);
  color: var(--text-secondary); font-size: 13px; font-weight: 500;
  text-decoration: none; transition: var(--transition);
}
.nav-item:hover { background: var(--bg-card); color: var(--text-primary); }
.nav-item.active {
  background: var(--bg-card); color: var(--text-primary);
  box-shadow: inset 3px 0 0 var(--amber);
}
.nav-icon { font-size: 14px; opacity: .7; width: 18px; text-align: center; }
.nav-spacer { flex: 1; }
.nav-footer {
  display: flex; align-items: center; gap: 8px;
  padding: 10px 14px; font-size: 12px; color: var(--text-muted);
  border-top: 1px solid var(--border); margin: 0 -12px -16px; padding: 12px 16px;
}
.nav-footer .dot {
  width: 8px; height: 8px; border-radius: 50%;
}
.nav-footer .dot.on { background: var(--green); box-shadow: 0 0 4px var(--green); }
.nav-footer .dot.off { background: var(--text-muted); }
.content {
  flex: 1; overflow-y: auto; padding: 28px 32px;
}
</style>
