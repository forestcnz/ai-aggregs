import { ref, watch, nextTick, onMounted } from 'vue'
import { startGateway, stopGateway, type GatewayStatus, type LogEntry } from '../../api/commands'

export function useDashboard(
  props: { status: GatewayStatus; logs: LogEntry[] },
  emit: (...args: any[]) => void
) {
  const logPanel = ref<HTMLElement | null>(null)
  const starting = ref(false)

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

  // 把 UNIX 时间戳（毫秒）格式化为本地时间字符串（YYYY-MM-DD HH:mm:ss.SSS）
  function formatTime(ts: number): string {
    const d = new Date(ts)
    const pad = (n: number, l = 2) => String(n).padStart(l, '0')
    return (
      `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
      `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`
    )
  }

  // 滚动日志面板到底部（展示最新日志）
  function scrollToBottom() {
    nextTick(() => {
      if (logPanel.value) logPanel.value.scrollTop = logPanel.value.scrollHeight
    })
  }

  // 切回该页时组件重新挂载（v-if），scrollTop 会归零 → 挂载后拉到底部
  onMounted(scrollToBottom)
  // 实时日志增长时自动滚动
  watch(() => props.logs.length, scrollToBottom)

  return { logPanel, starting, toggle, levelClass, formatTime }
}
