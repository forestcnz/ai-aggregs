import { ref, computed, onMounted, onActivated, watch } from 'vue'
import {
  getConfig,
  getProviderUsage,
  normalizeKey,
  maskKey,
  fmtNum,
  colorForModel,
  type UsageSummary,
  type Config
} from '../../api/commands'

export function useProviderUsage() {
  const config = ref<Config | null>(null)
  const summary = ref<UsageSummary | null>(null)
  const selectedProvider = ref<number>(0) // 0 = 全部
  const selectedKey = ref<string>('all') // 'all' = 全部
  const selectedDays = ref<number>(7)
  const loading = ref(false)

  // 当前选中供应商的 key 列表（computed 自动联动）
  const providerKeys = computed(() => {
    const pid = Number(selectedProvider.value)
    if (pid === 0 || !config.value) return []
    const p = config.value.providers.find((x) => x.id === pid)
    return p ? p.api_keys.map(normalizeKey).map((k) => k.key) : []
  })

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
      const pid = Number(selectedProvider.value)
      const providerId = pid === 0 ? null : pid
      const key = selectedKey.value === 'all' ? null : selectedKey.value
      const days = selectedDays.value === 0 ? null : selectedDays.value
      summary.value = await getProviderUsage(providerId, key, days)
    } catch (e) {
      console.error('getProviderUsage failed', e)
    } finally {
      loading.value = false
    }
  }

  // 供应商切换 → 重置 key 选择并重新加载
  watch(selectedProvider, () => {
    selectedKey.value = 'all'
    loadUsage()
  })
  watch([selectedKey, selectedDays], () => loadUsage())

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
    selectedProvider,
    selectedKey,
    selectedDays,
    providerKeys,
    loading,
    loadUsage,
    fmtNum,
    maskKey,
    colorForModel
  }
}
