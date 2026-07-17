import { ref, onMounted, onUnmounted } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  gatewayStatus,
  onGatewayStateChanged,
  onLog,
  type GatewayStatus,
  type LogEntry
} from './api/commands'

export function useApp() {
  const activeTab = ref<
    'dashboard' | 'providers' | 'chat' | 'usage' | 'provider-usage' | 'settings'
  >('dashboard')
  const status = ref<GatewayStatus>({ running: false, listen_addr: '' })
  const isMaximized = ref(false)
  let unlistenStatus: (() => void) | null = null
  let unlistenResize: (() => void) | null = null
  let unlistenLog: (() => void) | null = null

  // 日志状态提升到 App 层级，避免切换页面时组件卸载导致日志丢失
  const logs = ref<LogEntry[]>([])

  // 禁用浏览器右键菜单
  const preventCtx = (e: MouseEvent) => e.preventDefault()

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
    } catch {
      /* ignore */
    }
  }

  // 窗口控制
  async function minimize() {
    await appWindow.minimize()
  }
  async function toggleMaximize() {
    await appWindow.toggleMaximize()
  }
  async function closeWindow() {
    await appWindow.close()
  } // 触发 close_requested → 隐藏到托盘

  onMounted(async () => {
    document.addEventListener('contextmenu', preventCtx)

    await refreshStatus()
    await checkMaximized()
    unlistenStatus = await onGatewayStateChanged((running) => {
      status.value.running = running
      refreshStatus()
    })
    unlistenResize = await appWindow.onResized(() => checkMaximized())
    // 日志监听在 App 层级注册，整个应用生命周期内保持活跃
    unlistenLog = await onLog((entry) => {
      logs.value.push(entry)
      if (logs.value.length > 500) logs.value.shift()
    })
  })

  onUnmounted(() => {
    document.removeEventListener('contextmenu', preventCtx)
    unlistenStatus?.()
    unlistenResize?.()
    unlistenLog?.()
  })

  return {
    activeTab,
    status,
    isMaximized,
    logs,
    refreshStatus,
    minimize,
    toggleMaximize,
    closeWindow
  }
}
