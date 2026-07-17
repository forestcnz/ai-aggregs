import { ref, computed, onMounted } from 'vue'
import {
  opencodeConfigLoad,
  opencodeConfigSave,
  getConfig,
  maskKey,
  type OcForm,
  type OcProvider,
  type OcModel
} from '../../api/commands'
import { useDialog } from '../../composables/useDialog'

/** modalities 可选值（与 opencode schema 一致） */
export const MODALITY_OPTIONS = {
  input: ['text', 'image', 'audio', 'video', 'pdf'],
  output: ['text', 'image', 'audio']
} as const

/** npm 下拉预设项（与 opencode 常见 provider SDK 对齐） */
export const NPM_OPTIONS = [
  '@ai-sdk/openai-compatible',
  '@ai-sdk/openai',
  '@ai-sdk/anthropic',
  '@ai-sdk/google',
  '@ai-sdk/google-vertex',
  '@ai-sdk/azure',
  '@ai-sdk/mistral'
]

/** 返回 npm 下拉选项：预设项 + 当前值（若不在预设中则兜底追加，避免 select 回显空白） */
export function npmSelectOptions(current?: string | null): string[] {
  const opts = [...NPM_OPTIONS]
  if (current && !opts.includes(current)) opts.push(current)
  return opts
}

/** 创建一个空 model（默认值与 opencode schema 对齐） */
function emptyModel(id = ''): OcModel {
  return {
    id,
    name: id || null,
    attachment: false,
    reasoning: true,
    tool_call: true,
    modalities: { input: ['text'], output: ['text'] },
    limit: { context: 1000000, output: 131072 }
  }
}

/** 创建一个空 provider（baseURL 由调用方传入，默认取网关地址） */
function emptyProvider(id = '', baseUrl = ''): OcProvider {
  return {
    id,
    name: null,
    npm: null,
    options: { baseURL: baseUrl || null, apiKey: null },
    models: []
  }
}

/** 把网关 listen 地址（如 `127.0.0.1:8000`）规范化为 `http://127.0.0.1:8000/v1` */
function gatewayV1Url(listen: string): string {
  const addr = listen.trim()
  if (!addr) return ''
  const withScheme = addr.startsWith('http://') || addr.startsWith('https://')
    ? addr
    : `http://${addr}`
  return withScheme.endsWith('/v1') ? withScheme : `${withScheme}/v1`
}

export function useOpencodeConfig() {
  const { toast, alert: alertModal } = useDialog()

  const form = ref<OcForm>({ model: null, small_model: null, default_agent: null, providers: [] })
  const loading = ref(false)
  const saving = ref(false)
  const filePath = ref('')
  const fileExists = ref(false)
  /** 网关 baseURL（http://{listen}/v1），用于新建 provider 时预填 */
  const gatewayBaseUrl = ref('')

  // 折叠状态：用 provider / model 的引用做 key（避免 index 变动错位）
  const expandedProviders = ref<Set<OcProvider>>(new Set())
  const expandedModels = ref<Set<OcModel>>(new Set())
  /** apiKey 正在编辑的 provider（编辑态显示明文 input，否则展示 mask 值） */
  const editingKeys = ref<Set<OcProvider>>(new Set())

  /** 所有 provider 的所有 model，聚合成 `providerId/modelId` 形式（供主模型下拉）。
   * 若当前 model / small_model 值不在列表中（如 provider 已删），兜底包含进来，
   * 避免 <select> 找不到对应 option 而显示空白。 */
  const modelSelectOptions = computed(() => {
    const opts: string[] = []
    const seen = new Set<string>()
    const push = (v?: string | null) => {
      if (v && !seen.has(v)) {
        seen.add(v)
        opts.push(v)
      }
    }
    const providers = form.value?.providers
    if (Array.isArray(providers)) {
      for (const p of providers) {
        if (!p.id) continue
        for (const m of p.models ?? []) {
          if (!m.id) continue
          push(`${p.id}/${m.id}`)
        }
      }
    }
    push(form.value?.model)
    push(form.value?.small_model)
    return opts
  })

  /** 统计：provider 数 + model 总数 */
  const providerCount = computed(() => form.value?.providers?.length ?? 0)
  const modelTotalCount = computed(() =>
    (form.value?.providers ?? []).reduce((n, p) => n + (p.models?.length ?? 0), 0)
  )

  /** 从完整路径中提取文件名（如 opencode.jsonc / opencode.json） */
  const fileBaseName = computed(() => {
    const p = filePath.value
    if (!p) return 'opencode.jsonc'
    const parts = p.replace(/\\/g, '/').split('/')
    return parts[parts.length - 1] || 'opencode.jsonc'
  })

  async function load() {
    loading.value = true
    try {
      const res = await opencodeConfigLoad()
      filePath.value = res.path
      fileExists.value = res.exists
      // 同步读取网关 listen 地址，规范化为 http://{listen}/v1，供新建 provider 预填
      try {
        const cfg = await getConfig()
        gatewayBaseUrl.value = gatewayV1Url(cfg.listen)
      } catch {
        gatewayBaseUrl.value = ''
      }
      // 规范化：后端 OcForm 对空字段用了 skip_serializing_if，
      // 反序列化后 providers / 各 option 字段可能缺失，这里补全默认值避免模板遍历崩溃
      const f = res.form ?? ({} as OcForm)
      form.value = {
        model: f.model ?? null,
        small_model: f.small_model ?? null,
        default_agent: f.default_agent ?? null,
        providers: Array.isArray(f.providers) ? f.providers : []
      }
      expandedProviders.value.clear()
      expandedModels.value.clear()
    } catch (e) {
      await alertModal({ title: '读取失败', message: String(e) })
    } finally {
      loading.value = false
    }
  }

  async function save() {
    if (!form.value) return
    saving.value = true
    try {
      // 兜底：确保 providers 字段存在（用户可能从未触发过 load 的规范化）
      if (!Array.isArray(form.value.providers)) form.value.providers = []
      await opencodeConfigSave(form.value)
      toast('已保存 · 已备份', 'success')
      // 不调用 load()：避免重置折叠状态。文件已按 key 合并写回，表单即最新。
    } catch (e) {
      toast('保存失败: ' + String(e), 'error', 5000)
    } finally {
      saving.value = false
    }
  }

  // ---------- 折叠 ----------
  function toggleProvider(p: OcProvider) {
    const set = expandedProviders.value
    if (set.has(p)) set.delete(p)
    else set.add(p)
    // 触发响应式（Set 上的增删不自动触发）
    expandedProviders.value = new Set(set)
  }
  function toggleModel(m: OcModel) {
    const set = expandedModels.value
    if (set.has(m)) set.delete(m)
    else set.add(m)
    expandedModels.value = new Set(set)
  }

  // ---------- apiKey 编辑态 ----------
  /** 编辑草稿（进入编辑时清空，失焦后回写或回退） */
  const keyDraft = ref('')
  function startEditKey(p: OcProvider) {
    // 清空输入框，方便用户重新输入完整 key（不预填旧值）
    keyDraft.value = ''
    editingKeys.value = new Set(editingKeys.value).add(p)
    editingKeys.value = new Set(editingKeys.value)
  }
  function endEditKey(p: OcProvider) {
    // 草稿非空才回写；空则回退原值（展示态回显旧值或「点击设置…」）
    const v = keyDraft.value.trim()
    if (v) p.options.apiKey = v
    const set = editingKeys.value
    set.delete(p)
    editingKeys.value = new Set(set)
  }
  /** 展示态用的 mask 值（统一用全局 maskKey，短 key 也首尾都露） */
  function maskedKey(p: OcProvider): string {
    const k = p.options.apiKey
    if (!k) return ''
    return maskKey(k)
  }

  // ---------- provider 增删 ----------
  function addProvider() {
    const p = emptyProvider('', gatewayBaseUrl.value)
    form.value.providers.push(p)
    expandedProviders.value = new Set(expandedProviders.value).add(p)
    expandedProviders.value = new Set(expandedProviders.value)
  }
  function removeProvider(i: number) {
    form.value.providers.splice(i, 1)
  }

  // ---------- model 增删 ----------
  function addModel(p: OcProvider) {
    const m = emptyModel()
    p.models.push(m)
    expandedModels.value = new Set(expandedModels.value).add(m)
    expandedModels.value = new Set(expandedModels.value)
  }
  function removeModel(p: OcProvider, i: number) {
    p.models.splice(i, 1)
  }

  // ---------- modalities 多选切换 ----------
  function toggleModality(m: OcModel, side: 'input' | 'output', val: string) {
    const arr = m.modalities[side]
    const idx = arr.indexOf(val)
    if (idx >= 0) arr.splice(idx, 1)
    else arr.push(val)
  }

  /** model 折叠态下的标志位预览（reasoning/tool/attach） */
  function modelFlags(m: OcModel) {
    const flags: { label: string; on: boolean }[] = [
      { label: 'reasoning', on: m.reasoning },
      { label: 'tool', on: m.tool_call },
      { label: 'attach', on: m.attachment }
    ]
    return flags.filter((f) => f.on)
  }

  onMounted(load)

  return {
    // 状态
    form,
    loading,
    saving,
    filePath,
    fileExists,
    expandedProviders,
    expandedModels,
    // 计算属性
    modelSelectOptions,
    providerCount,
    modelTotalCount,
    fileBaseName,
    // 动作
    load,
    save,
    // 折叠
    toggleProvider,
    toggleModel,
    // apiKey 编辑态
    editingKeys,
    keyDraft,
    startEditKey,
    endEditKey,
    maskedKey,
    // 增删
    addProvider,
    removeProvider,
    addModel,
    removeModel,
    // modalities
    toggleModality,
    modelFlags
  }
}
