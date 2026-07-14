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

    <!-- 统计卡片 -->
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
.page-title { font-size: 22px; font-weight: 700; margin-bottom: 4px; }
.page-sub { font-size: 13px; color: var(--text-secondary); margin-bottom: 24px; }
.stats { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin-bottom: 20px; }
.stat-card {
  background: var(--bg-card); border-radius: var(--radius-md);
  border: 1px solid var(--border); padding: 18px 20px;
}
.stat-card .label {
  font-size: 12px; font-weight: 500; color: var(--text-muted);
  text-transform: uppercase; letter-spacing: .5px; display: block; margin-bottom: 4px;
}
.stat-card .value { font-size: 24px; font-weight: 700; }
.stat-card .value.running { color: var(--green); }
.stat-card .value.stopped { color: var(--red); }
.stat-card .value.mono { font-family: 'JetBrains Mono', monospace; font-size: 18px; }
.control-card {
  background: var(--bg-card); border-radius: var(--radius-md);
  border: 1px solid var(--border); padding: 20px 24px;
  display: flex; align-items: center; justify-content: space-between;
  margin-bottom: 24px;
}
.status-info { display: flex; flex-direction: column; gap: 6px; }
.addr { font-size: 13px; color: var(--text-secondary); font-family: monospace; }
.btn.sm { padding: 4px 12px; font-size: 12px; }
.log-section { }
.log-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
.log-header h3 { font-size: 14px; font-weight: 600; }
.log-panel {
  background: var(--bg-deep); border-radius: var(--radius-md);
  border: 1px solid var(--border); padding: 14px;
  height: 280px; overflow-y: auto; font-family: 'JetBrains Mono', monospace; font-size: 12px;
  line-height: 1.7;
}
.log-empty { color: var(--text-muted); text-align: center; padding: 40px; }
.log-line { display: flex; gap: 8px; }
.log-line .level {
  flex-shrink: 0; width: 50px; font-weight: 600; font-size: 11px; text-transform: uppercase;
}
.log-line .level.info { color: var(--blue); }
.log-line .level.warn { color: #ea580c; }
.log-line .level.error { color: var(--red); }
.log-line .level.trace, .log-line .level.debug { color: var(--text-muted); }
.log-line .msg { color: var(--text-secondary); word-break: break-all; }
</style>
