import { ref, watch, nextTick } from 'vue'
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

  // 监听 logs 变化，自动滚动到底部
  watch(
    () => props.logs.length,
    () => {
      nextTick(() => {
        if (logPanel.value) logPanel.value.scrollTop = logPanel.value.scrollHeight
      })
    }
  )

  return { logPanel, starting, toggle, levelClass, formatTime }
}
