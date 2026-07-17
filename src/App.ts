import { ref, onMounted, onUnmounted } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import {
  gatewayStatus,
  onGatewayStateChanged,
  onLog,
  opencodeVersion,
  claudeCodeVersion,
  autostartGatewayIfConfigured,
  type GatewayStatus,
  type LogEntry
} from './api/commands'

export function useApp() {
  const activeTab = ref<
    | 'dashboard'
    | 'providers'
    | 'chat'
    | 'usage'
    | 'provider-usage'
    | 'settings'
    | 'opencode'
    | 'claude-code'
  >('dashboard')
  const status = ref<GatewayStatus>({ running: false, listen_addr: '' })
  const isMaximized = ref(false)
  /** opencode 版本号；null 表示未安装/未检测到（侧边栏入口据此显隐） */
  const ocVersion = ref<string | null>(null)
  /** claude code 版本号；null 表示未安装/未检测到（侧边栏入口据此显隐） */
  const ccVersion = ref<string | null>(null)
  /** 启动期检测是否完成（网关状态 + opencode/claude code 版本）。
   * 完成前不渲染侧边栏导航，让 opencode/claude-code 入口一次性出现而非先后弹出。 */
  const ready = ref(false)
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

    // 启动期独立检测并行执行（网关状态、窗口最大化、opencode/claude code 版本）：
    // 先「提前判断」两个工具是否存在，再一次性渲染侧边栏，入口不会先后弹出。
    // 每个检测各自吞掉异常（未安装→版本置 null），不影响其它检测。
    const ocCheck = opencodeVersion()
      .then((v) => (ocVersion.value = v))
      .catch(() => (ocVersion.value = null))
    const ccCheck = claudeCodeVersion()
      .then((v) => (ccVersion.value = v))
      .catch(() => (ccVersion.value = null))

    await Promise.all([refreshStatus(), checkMaximized(), ocCheck, ccCheck])
    ready.value = true

    // 事件监听在首轮状态拉取之后注册，避免初始事件与本地状态竞争
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

    // 页面就绪、事件监听已注册后，再按配置自动恢复网关。
    // 启动后刷新一次状态（状态变化事件也会驱动刷新，此处兜底）。
    try {
      await autostartGatewayIfConfigured()
    } catch (e) {
      console.error('autostart gateway failed', e)
    }
    await refreshStatus()
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
    ocVersion,
    ccVersion,
    ready,
    logs,
    refreshStatus,
    minimize,
    toggleMaximize,
    closeWindow
  }
}
