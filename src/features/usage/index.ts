import { ref, onMounted, onActivated, watch } from 'vue'
import { getConfig, getUsage, fmtNum, colorForModel, type UsageSummary, type Config } from '../../api/commands'

export function useUsage() {
  const config = ref<Config | null>(null)
  const summary = ref<UsageSummary | null>(null)
  const selectedKey = ref<string>('all')
  const selectedDays = ref<number>(7)
  const loading = ref(false)

  async function loadConfig() {
    try {
      config.value = await getConfig()
    } catch (e) {
      console.error('getConfig failed', e)
    }
  }

  async function loadUsage() {
    loading.value = true
    try {
      const key = selectedKey.value === 'all' ? null : selectedKey.value
      const days = selectedDays.value === 0 ? null : selectedDays.value
      summary.value = await getUsage(key, days)
    } catch (e) {
      console.error('getUsage failed', e)
    } finally {
      loading.value = false
    }
  }

  // 筛选变化时重新加载
  watch([selectedKey, selectedDays], () => {
    loadUsage()
  })

  onMounted(async () => {
    await loadConfig()
    await loadUsage()
  })

  // KeepAlive 组件重新激活时刷新数据（切换 tab 回来时实时获取）
  onActivated(() => {
    loadConfig()
    loadUsage()
  })

  return {
    config,
    summary,
    selectedKey,
    selectedDays,
    loading,
    loadUsage,
    fmtNum,
    colorForModel
  }
}
