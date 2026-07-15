import { ref, onMounted, watch } from 'vue'
import { getConfig, getUsage, type UsageSummary, type Config } from '../../api/commands'

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

  // 格式化 token 数字（K / M）
  function fmtNum(n: number): string {
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M'
    if (n >= 1_000) return (n / 1_000).toFixed(1) + 'K'
    return String(n)
  }

  // 遮蔽 consumer key 中间部分
  function maskKey(key: string): string {
    if (key.length <= 10) return key
    return key.slice(0, 6) + '...' + key.slice(-4)
  }

  // 模型配色（克制调色板）
  const palette = [
    '#1f1e1e',
    '#646363',
    '#7c3aed',
    '#03b000',
    '#c0703a',
    '#2d7d8c',
    '#8b4513',
  ]

  function colorForModel(index: number): string {
    return palette[index % palette.length]
  }

  // 筛选变化时重新加载
  watch([selectedKey, selectedDays], () => {
    loadUsage()
  })

  onMounted(async () => {
    await loadConfig()
    await loadUsage()
  })

  return {
    config,
    summary,
    selectedKey,
    selectedDays,
    loading,
    loadUsage,
    fmtNum,
    maskKey,
    colorForModel,
  }
}
