<script setup lang="ts">
import { useApp } from './App'
import GatewayStatusView from './features/dashboard/index.vue'
import ProviderList from './features/providers/index.vue'
import ConfigEditor from './features/settings/index.vue'
import ChatView from './features/chat/index.vue'
import UsageView from './features/usage/index.vue'
import ProviderUsageView from './features/provider-usage/index.vue'

const { activeTab, status, isMaximized, logs, refreshStatus, minimize, toggleMaximize, closeWindow } = useApp()
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
      <!-- 侧边栏 -->
      <nav class="sidebar">
        <a
          v-for="tab in [
            { id: 'dashboard', label: '网关状态' },
            { id: 'providers', label: '供应商' },
            { id: 'chat', label: 'AI聊天' },
            { id: 'usage', label: '用量统计' },
            { id: 'provider-usage', label: '供量统计' },
            { id: 'settings', label: '设置' }
          ]"
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
            <line x1="2.5" y1="11.5" x2="2.5" y2="8.5" stroke="currentColor" stroke-width="1.8" stroke-linecap="square" />
            <line x1="6" y1="11.5" x2="6" y2="4.5" stroke="currentColor" stroke-width="1.8" stroke-linecap="square" />
            <line x1="9.5" y1="11.5" x2="9.5" y2="6.5" stroke="currentColor" stroke-width="1.8" stroke-linecap="square" />
            <line x1="1.5" y1="12.5" x2="12.5" y2="12.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="square" />
          </svg>
          <svg v-else-if="tab.id === 'provider-usage'" class="nav-icon" viewBox="0 0 14 14" fill="none">
            <rect x="1.5" y="1.5" width="4.5" height="4.5" rx="1" stroke="currentColor" stroke-width="1.3" />
            <line x1="8" y1="3.5" x2="12.5" y2="3.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="square" />
            <line x1="3.5" y1="8" x2="3.5" y2="12.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="square" />
            <rect x="7" y="7" width="5.5" height="5.5" rx="1" stroke="currentColor" stroke-width="1.3" />
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

      <!-- 内容区 -->
      <main class="content" :class="{ 'content-flush': activeTab === 'dashboard' }">
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
        <ConfigEditor v-else />
      </main>
    </div>
  </div>
</template>

<style src="./App.css" scoped></style>
