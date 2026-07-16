import { ref, onMounted, onUnmounted } from 'vue'
import {
  getConfig,
  saveConfig,
  runtimeStatus,
  toggleProvider,
  toggleKey,
  type Config,
  type ProviderConfig,
  type ProviderRuntime
} from '../../api/commands'

export function useProviderList() {
  const config = ref<Config | null>(null)
  const runtimeMap = ref<Map<string, ProviderRuntime>>(new Map())
  const loading = ref(false)
  const msg = ref('')
  let timer: ReturnType<typeof setInterval> | null = null

  // 上次拉取运行时状态的时间戳
  const lastRuntimeAt = ref(Date.now())

  // 弹窗状态
  const modalMode = ref<'add' | 'edit' | null>(null)
  const editingProvider = ref<ProviderConfig>(blankProvider())
  const editingIdx = ref(-1)
  const modelInput = ref('')
  const keyInput = ref('')

  function blankProvider(): ProviderConfig {
    return {
      id: 0,
      name: '',
      protocol: 'chat',
      base_url: '',
      api_keys: [],
      models: [],
      timeout_secs: 3000,
      extra_headers: {},
      enabled: true,
      reasoning_effort: null
    }
  }

  // ---- 数据 ----

  async function refresh() {
    config.value = await getConfig()
    await refreshRuntime()
  }

  async function refreshRuntime() {
    try {
      const rt = await runtimeStatus()
      runtimeMap.value = new Map(rt.map((p) => [p.name, p]))
      lastRuntimeAt.value = Date.now()
    } catch {
      /* ignore */
    }
  }

  function showMsg(text: string) {
    msg.value = text
    setTimeout(() => {
      msg.value = ''
    }, 3000)
  }

  async function save() {
    if (!config.value) return
    try {
      await saveConfig(config.value)
      await refreshRuntime()
      showMsg('已保存')
    } catch (e) {
      showMsg('保存失败: ' + String(e))
    }
  }

  // ---- 卡片操作 ----

  async function onToggleProvider(idx: number, enabled: boolean) {
    if (!config.value) return
    loading.value = true
    try {
      await toggleProvider(config.value.providers[idx].name, enabled)
      config.value.providers[idx].enabled = enabled
      await refreshRuntime()
    } catch (e) {
      showMsg(String(e))
    } finally {
      loading.value = false
    }
  }

  // 切换单个 key 的启用状态
  async function onToggleKey(providerName: string, keyIdx: number, enabled: boolean) {
    if (!config.value) return
    loading.value = true
    try {
      await toggleKey(providerName, keyIdx, enabled)
      // 同步前端 config 中对应 key 的 enabled 状态
      const provider = config.value.providers.find((p) => p.name === providerName)
      if (provider) {
        const entry = provider.api_keys[keyIdx]
        if (typeof entry === 'string') {
          provider.api_keys[keyIdx] = { key: entry, enabled }
        } else {
          entry.enabled = enabled
        }
      }
      await refreshRuntime()
    } catch (e) {
      showMsg(String(e))
    } finally {
      loading.value = false
    }
  }

  // ---- 弹窗 ----

  function openAdd() {
    editingProvider.value = blankProvider()
    editingIdx.value = -1
    modelInput.value = ''
    keyInput.value = ''
    modalMode.value = 'add'
  }

  function openEdit(idx: number) {
    if (!config.value) return
    const p = config.value.providers[idx]
    editingProvider.value = JSON.parse(JSON.stringify(p)) // 深拷贝，编辑不影响原数据
    editingIdx.value = idx
    modelInput.value = ''
    keyInput.value = ''
    modalMode.value = 'edit'
  }

  function closeModal() {
    modalMode.value = null
  }

  function onDocumentKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && modalMode.value) {
      closeModal()
    }
  }

  function submitModal() {
    const p = editingProvider.value
    if (!p.name.trim() || !p.base_url.trim()) {
      showMsg('名称和 Base URL 不能为空')
      return
    }
    if (!config.value) return

    if (modalMode.value === 'add') {
      config.value.providers.push({ ...p, name: p.name.trim() })
    } else if (modalMode.value === 'edit' && editingIdx.value >= 0) {
      config.value.providers[editingIdx.value] = { ...p, name: p.name.trim() }
    }
    modalMode.value = null
    save()
  }

  function deleteFromModal() {
    if (editingIdx.value < 0 || !config.value) {
      modalMode.value = null
      return
    }
    config.value.providers.splice(editingIdx.value, 1)
    modalMode.value = null
    save()
  }

  // ---- 弹窗内 model / key ----

  function modalAddModel() {
    const v = modelInput.value.trim()
    if (!v || editingProvider.value.models.includes(v)) return
    editingProvider.value.models.push(v)
    modelInput.value = ''
  }
  function modalRemoveModel(i: number) {
    editingProvider.value.models.splice(i, 1)
  }

  function modalAddKey() {
    const v = keyInput.value.trim()
    if (!v) return
    editingProvider.value.api_keys.push({ key: v, enabled: true })
    keyInput.value = ''
  }
  function modalRemoveKey(i: number) {
    editingProvider.value.api_keys.splice(i, 1)
  }

  // ---- 辅助 ----

  function getRuntime(name: string) {
    return runtimeMap.value.get(name)
  }

  // 按 idx 从 runtime 中取某个 key 的运行时状态（用于黑名单/禁用）
  function keyRuntime(name: string, idx: number) {
    return getRuntime(name)?.keys.find((k) => k.idx === idx)
  }

  function maskKey(key: string): string {
    if (key.length <= 12) return key.slice(0, 4) + '**'
    return key.slice(0, 6) + '**' + key.slice(-6)
  }

  // 按协议返回图标 emoji
  function iconFor(protocol: string): string {
    switch (protocol) {
      case 'anthropic':
        return '🧠'
      case 'responses':
        return '🔄'
      default:
        return '💬'
    }
  }

  // 基于后端快照计算 key 的黑名单解除时刻
  function keyReleaseTime(blacklistRemainingSecs: number | null | undefined): Date | null {
    const base = blacklistRemainingSecs ?? 0
    if (base <= 0) return null
    return new Date(lastRuntimeAt.value + base * 1000)
  }

  // 格式化为 年/月/日 时:分:秒
  function fmtTime(d: Date): string {
    const pad = (n: number) => String(n).padStart(2, '0')
    return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
  }

  onMounted(() => {
    refresh()
    timer = setInterval(refreshRuntime, 5000)
    document.addEventListener('keydown', onDocumentKeydown)
  })
  onUnmounted(() => {
    if (timer) clearInterval(timer)
    document.removeEventListener('keydown', onDocumentKeydown)
  })

  return {
    config, loading, msg,
    modalMode, editingProvider, modelInput, keyInput,
    onToggleProvider, onToggleKey,
    openAdd, openEdit, closeModal, submitModal, deleteFromModal,
    modalAddModel, modalRemoveModel, modalAddKey, modalRemoveKey,
    getRuntime, keyRuntime, maskKey, iconFor, keyReleaseTime, fmtTime
  }
}
