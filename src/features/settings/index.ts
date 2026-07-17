import { ref, computed, onMounted } from 'vue'
import {
  getConfig,
  saveConfig,
  enableAutostart,
  disableAutostart,
  autostartStatus,
  getLastUsedModels,
  maskKey,
  type Config
} from '../../api/commands'
import { useDialog } from '../../composables/useDialog'

export function useSettings() {
  const { toast, alert: alertModal } = useDialog()
  const cfg = ref<Config | null>(null)
  const saving = ref(false)
  const autoStart = ref(false)
  const keyInput = ref('')
  /** 模型映射：每条规则的「实际模型」chip 输入框文本（与 model_mappings 并行） */
  const mapInputs = ref<string[]>([])
  /** 别名 → 上次成功响应的实际模型（仅内存，进入设置页时拉取一次，不自动刷新） */
  const lastUsed = ref<Record<string, string>>({})

  function addKey() {
    if (!cfg.value) return
    const v = keyInput.value.trim()
    if (!v) return
    cfg.value.consumer.api_keys.push(v)
    keyInput.value = ''
  }
  function removeKey(i: number) {
    if (!cfg.value) return
    cfg.value.consumer.api_keys.splice(i, 1)
  }
  // maskKey 统一使用 api/commands.ts 中的实现

  async function load() {
    try {
      cfg.value = await getConfig()
      autoStart.value = await autostartStatus()
      mapInputs.value = cfg.value.model_mappings.map(() => '')
      lastUsed.value = await getLastUsedModels().catch(() => ({}))
    } catch (e) {
      console.error(e)
    }
  }

  async function save() {
    if (!cfg.value) return
    saving.value = true
    try {
      await saveConfig(cfg.value)
      toast('配置已保存', 'success')
    } catch (e) {
      toast('保存失败: ' + String(e), 'error', 5000)
    } finally {
      saving.value = false
    }
  }

  async function toggleAutostart(val: boolean) {
    try {
      if (val) await enableAutostart()
      else await disableAutostart()
      autoStart.value = val
    } catch (e) {
      await alertModal({ title: '操作失败', message: String(e) })
    }
  }

  // ---- 模型映射 ----
  function addMapping() {
    if (!cfg.value) return
    cfg.value.model_mappings.push({ alias: '', models: [], enabled: true })
    mapInputs.value.push('')
  }
  function removeMapping(i: number) {
    if (!cfg.value) return
    cfg.value.model_mappings.splice(i, 1)
    mapInputs.value.splice(i, 1)
  }
  function addMapModel(i: number) {
    if (!cfg.value) return
    const v = (mapInputs.value[i] ?? '').trim()
    if (!v) return
    const pool = cfg.value.model_mappings[i].models
    if (!pool.includes(v)) pool.push(v)
    mapInputs.value[i] = ''
  }
  function removeMapModel(i: number, mi: number) {
    if (!cfg.value) return
    cfg.value.model_mappings[i].models.splice(mi, 1)
  }
  /** 重复别名集合（同一别名出现在 2 条及以上规则中） */
  const duplicateAliases = computed(() => {
    const counts = new Map<string, number>()
    if (!cfg.value) return new Set<string>()
    for (const mm of cfg.value.model_mappings) {
      const a = mm.alias.trim()
      if (a) counts.set(a, (counts.get(a) ?? 0) + 1)
    }
    return new Set([...counts.entries()].filter(([, n]) => n > 1).map(([a]) => a))
  })
  function isDuplicateAlias(alias: string): boolean {
    return duplicateAliases.value.has(alias.trim())
  }
  /** 该别名池中的某个模型是否为上次成功响应的模型（用于蓝色高亮） */
  function isLastUsed(alias: string, model: string): boolean {
    const a = alias.trim()
    return !!a && lastUsed.value[a] === model
  }

  onMounted(load)

  return {
    cfg,
    saving,
    autoStart,
    keyInput,
    addKey,
    removeKey,
    maskKey,
    save,
    toggleAutostart,
    mapInputs,
    addMapping,
    removeMapping,
    addMapModel,
    removeMapModel,
    isDuplicateAlias,
    isLastUsed
  }
}
