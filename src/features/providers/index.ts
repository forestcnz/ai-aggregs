import { ref, computed, onMounted, onUnmounted } from 'vue'
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

  // 卡片展示顺序：直接按 config.providers 数组顺序（用户可拖动调整顺序）
  const sortedProviders = computed(() => {
    if (!config.value) return [] as { p: ProviderConfig; idx: number }[]
    return config.value.providers.map((p, idx) => ({ p, idx }))
  })

  // ---- 拖拽排序状态（纯鼠标事件实现，兼容 Tauri webview）----
  // dragIdx：正在拖动的源卡片下标（-1 表示未拖动）
  // dragOverIdx：当前鼠标悬停的目标卡片下标
  // pendingDragIdx：手柄 mousedown 时记录，移动超过阈值后才转为正式拖动，避免点击误判
  const dragIdx = ref(-1)
  const dragOverIdx = ref(-1)
  const pendingDragIdx = ref(-1)

  // 弹窗状态
  const modalMode = ref<'add' | 'edit' | null>(null)
  const editingProvider = ref<ProviderConfig>(blankProvider())
  const editingIdx = ref(-1)
  const modelInput = ref('')
  const keyInput = ref('')
  // 自定义请求头输入：格式 "Key: Value"，回车添加
  const headerInput = ref('')

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
    headerInput.value = ''
    modalMode.value = 'add'
  }

  function openEdit(idx: number) {
    if (!config.value) return
    const p = config.value.providers[idx]
    editingProvider.value = JSON.parse(JSON.stringify(p)) // 深拷贝，编辑不影响原数据
    editingIdx.value = idx
    modelInput.value = ''
    keyInput.value = ''
    headerInput.value = ''
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

  // ---- 弹窗内 自定义请求头 ----
  // headerInput 格式 "Key: Value"，回车解析添加
  function modalAddHeader() {
    const raw = headerInput.value.trim()
    if (!raw) return
    const sep = raw.indexOf(':')
    if (sep <= 0) {
      showMsg('格式：Key: Value')
      return
    }
    const k = raw.slice(0, sep).trim()
    const v = raw.slice(sep + 1).trim()
    if (!k) return
    editingProvider.value.extra_headers[k] = v
    headerInput.value = ''
  }
  function modalRemoveHeader(key: string) {
    delete editingProvider.value.extra_headers[key]
  }

  // ---- 拖拽排序（纯鼠标事件，绕开 HTML5 DnD，兼容 Tauri webview）----

  // 记录 mousedown 起点坐标，用于判断移动是否超过阈值
  let dragStartX = 0
  let dragStartY = 0

  // 通过鼠标坐标查找当前悬停的卡片下标（卡片元素带 data-idx 属性）
  function findCardIdxAtPoint(clientX: number, clientY: number): number {
    const el = document.elementFromPoint(clientX, clientY)
    if (!el) return -1
    const card = (el as HTMLElement).closest('.provider-card')
    if (!card) return -1
    const attr = card.getAttribute('data-idx')
    return attr === null ? -1 : Number(attr)
  }

  // 手柄按下：记录起点，挂载全局 mousemove / mouseup 监听
  function onHandleMouseDown(e: MouseEvent, idx: number) {
    if (e.button !== 0) return // 仅响应左键
    pendingDragIdx.value = idx
    dragStartX = e.clientX
    dragStartY = e.clientY
    document.addEventListener('mousemove', onDocMouseMove)
    document.addEventListener('mouseup', onDocMouseUp)
  }

  function onDocMouseMove(e: MouseEvent) {
    // 首次移动需超过阈值（4px）才正式进入拖动状态，避免单击被误判为拖动
    if (pendingDragIdx.value !== -1) {
      const dx = e.clientX - dragStartX
      const dy = e.clientY - dragStartY
      if (dx * dx + dy * dy < 16) return
      dragIdx.value = pendingDragIdx.value
      pendingDragIdx.value = -1
    }
    if (dragIdx.value === -1) return
    e.preventDefault() // 阻止文字/图片被选中
    dragOverIdx.value = findCardIdxAtPoint(e.clientX, e.clientY)
  }

  async function onDocMouseUp() {
    document.removeEventListener('mousemove', onDocMouseMove)
    document.removeEventListener('mouseup', onDocMouseUp)
    const from = dragIdx.value
    const to = dragOverIdx.value
    dragIdx.value = -1
    dragOverIdx.value = -1
    pendingDragIdx.value = -1
    if (from === -1 || to === -1 || from === to) return
    if (!config.value) return
    // 重排 providers 数组（后端按数组下标持久化 sort_order）
    const arr = config.value.providers
    const [moved] = arr.splice(from, 1)
    arr.splice(to, 0, moved)
    await save()
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
    // 组件卸载时移除可能残留的拖拽监听
    document.removeEventListener('mousemove', onDocMouseMove)
    document.removeEventListener('mouseup', onDocMouseUp)
  })

  return {
    config, loading, msg,
    sortedProviders,
    dragIdx, dragOverIdx,
    onHandleMouseDown,
    modalMode, editingProvider, modelInput, keyInput, headerInput,
    onToggleProvider, onToggleKey,
    openAdd, openEdit, closeModal, submitModal, deleteFromModal,
    modalAddModel, modalRemoveModel, modalAddKey, modalRemoveKey,
    modalAddHeader, modalRemoveHeader,
    getRuntime, keyRuntime, maskKey, iconFor, keyReleaseTime, fmtTime
  }
}
