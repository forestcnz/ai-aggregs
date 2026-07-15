<script setup lang="ts">
import { useDashboard } from './index'

const props = defineProps<{ status: GatewayStatus; logs: LogEntry[] }>()
const emit = defineEmits<{ changed: []; 'clear-logs': [] }>()
import { type GatewayStatus, type LogEntry } from '../../api/commands'

const { logPanel, starting, toggle, levelClass, formatTime } = useDashboard(props, emit)
</script>

<template>
  <div class="dashboard-root">
    <h1 class="page-title">网关状态</h1>
    <p class="page-sub">管理 API 聚合网关的运行</p>

    <!-- 统计卡片 — 1px 分隔三栏 -->
    <div class="stats">
      <div class="stat-card">
        <span class="label">网关</span>
        <span class="value" :class="status.running ? 'running' : 'stopped'">
          {{ status.running ? '运行中' : '已停止' }}
        </span>
      </div>
      <div class="stat-card">
        <span class="label">监听地址</span>
        <span class="value mono">{{ status.listen_addr || '—' }}</span>
      </div>
    </div>

    <!-- 控制按钮 -->
    <div class="control-card">
      <div class="status-info">
        <div class="badge" :class="status.running ? 'running' : 'stopped'">
          <div class="dot"></div>
          {{ status.running ? '运行中' : '已停止' }}
        </div>
      </div>
      <button
        class="btn"
        :class="status.running ? 'btn-stop' : 'btn-start'"
        :disabled="starting"
        @click="toggle"
      >
        {{ starting ? '处理中...' : status.running ? '停止网关' : '启动网关' }}
      </button>
    </div>

    <!-- 日志面板 -->
    <div class="log-section">
      <div class="log-header">
        <h3>运行日志</h3>
        <button class="btn btn-secondary sm" @click="emit('clear-logs')">清除</button>
      </div>
      <div ref="logPanel" class="log-panel">
        <div v-if="logs.length === 0" class="log-empty">暂无日志</div>
        <div v-for="(log, i) in logs" :key="i" class="log-line">
          <span class="time">{{ formatTime(log.ts) }}</span>
          <span :class="['level', levelClass(log.level)]">{{ log.level }}</span>
          <span class="msg">{{ log.message }}</span>
        </div>
      </div>
    </div>
  </div>
</template>

<style src="./index.css" scoped></style>
