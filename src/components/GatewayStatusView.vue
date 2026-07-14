<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick } from 'vue'
import {
  startGateway, stopGateway, onLog,
  type GatewayStatus, type LogEntry,
} from '../api/commands'

const props = defineProps<{ status: GatewayStatus }>()
const emit = defineEmits<{ changed: [] }>()

const logs = ref<LogEntry[]>([])
const logPanel = ref<HTMLElement | null>(null)
const starting = ref(false)
let unlisten: (() => void) | null = null

async function toggle() {
  starting.value = true
  try {
    if (props.status.running) {
      await stopGateway()
    } else {
      await startGateway()
    }
    emit('changed')
  } catch (e) {
    console.error(e)
  } finally {
    starting.value = false
  }
}

function levelClass(level: string): string {
  return level.toLowerCase()
}

onMounted(async () => {
  unlisten = await onLog((entry) => {
    logs.value.push(entry)
    if (logs.value.length > 500) logs.value.shift()
    nextTick(() => {
      if (logPanel.value) logPanel.value.scrollTop = logPanel.value.scrollHeight
    })
  })
})

onUnmounted(() => { unlisten?.() })
</script>

<template>
  <div>
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
        <span class="addr" v-if="status.running">http://{{ status.listen_addr }}</span>
      </div>
      <button class="btn" :class="status.running ? 'btn-stop' : 'btn-start'" :disabled="starting" @click="toggle">
        {{ starting ? '处理中...' : (status.running ? '停止网关' : '启动网关') }}
      </button>
    </div>

    <!-- 日志面板 -->
    <div class="log-section">
      <div class="log-header">
        <h3>运行日志</h3>
        <button class="btn btn-secondary sm" @click="logs = []">清除</button>
      </div>
      <div class="log-panel" ref="logPanel">
        <div v-if="logs.length === 0" class="log-empty">暂无日志</div>
        <div v-for="(log, i) in logs" :key="i" class="log-line">
          <span :class="['level', levelClass(log.level)]">{{ log.level }}</span>
          <span class="msg">{{ log.message }}</span>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page-title { font-size: 20px; font-weight: 600; color: var(--text-strong); letter-spacing: -.01em; }
.page-sub { font-size: 12px; color: var(--text-weak); margin-top: 2px; margin-bottom: 22px; }

/* 统计卡 — 1px 分隔，无外框包裹 */
.stats { display: grid; grid-template-columns: 1fr 1fr; gap: 0; border: 1px solid var(--border-weak); border-radius: var(--r-md); margin-bottom: 16px; overflow: hidden; }
.stat-card {
  padding: 14px 16px; border-right: 1px solid var(--border-weak);
}
.stat-card:last-child { border-right: none; }
.stat-card .label {
  font-size: 10px; font-weight: 400; color: var(--text-weak);
  text-transform: uppercase; letter-spacing: .1em; display: block; margin-bottom: 6px;
}
.stat-card .value { font-size: 20px; font-weight: 500; color: var(--text-strong); }
.stat-card .value.running { color: var(--green); }
.stat-card .value.stopped { color: var(--text-weak); }
.stat-card .value.mono { font-size: 16px; font-weight: 400; }

/* 控制卡 */
.control-card {
  border: 1px solid var(--border-weak); border-radius: var(--r-md);
  padding: 16px 20px; display: flex; align-items: center; justify-content: space-between;
  margin-bottom: 20px;
}
.status-info { display: flex; flex-direction: column; gap: 6px; }
.addr { font-size: 12px; color: var(--text); }
.btn.sm { padding: 5px 10px; font-size: 12px; }

/* 日志面板 */
.log-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 10px; }
.log-header h3 { font-size: 13px; font-weight: 600; color: var(--text-strong); }
.log-panel {
  background: var(--bg-weak); border: 1px solid var(--border-weak); border-radius: var(--r-md);
  padding: 12px 14px; height: 240px; overflow-y: auto; font-size: 11px; line-height: 1.9;
}
.log-empty { color: var(--text-weak); text-align: center; padding: 40px; }
.log-line { display: flex; gap: 10px; }
.log-line .level {
  flex-shrink: 0; width: 42px; font-weight: 600; font-size: 10px; text-transform: uppercase;
}
.log-line .level.info { color: var(--text-strong); }
.log-line .level.warn { color: var(--green); }
.log-line .level.error { color: var(--red); }
.log-line .level.trace, .log-line .level.debug { color: var(--text-weak); }
.log-line .msg { color: var(--text); word-break: break-all; }
</style>
