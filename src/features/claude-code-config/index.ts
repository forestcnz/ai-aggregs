import { ref, computed, onMounted } from 'vue'
import {
  claudeCodeConfigLoad,
  claudeCodeConfigSave,
  getConfig,
  maskKey,
  type CcForm,
  type CcEnvEntry
} from '../../api/commands'
import { useDialog } from '../../composables/useDialog'

/** 精选「连接」字段 key */
export const CC_BASE_URL_KEY = 'ANTHROPIC_BASE_URL'
export const CC_AUTH_TOKEN_KEY = 'ANTHROPIC_AUTH_TOKEN'

/** 精选「模型」字段（用 ModelCombobox 选网关 consumer 模型）。
 * 取自官方「模型配置」文档的别名 pinning 环境变量族：
 * `ANTHROPIC_MODEL` 为当前模型；`ANTHROPIC_DEFAULT_{FABLE,OPUS,SONNET,HAIKU}_MODEL`
 * 决定 fable/opus/sonnet/haiku 别名解析到哪个后端模型；`CLAUDE_CODE_SUBAGENT_MODEL`
 * 为子 agent。注：`ANTHROPIC_SMALL_FAST_MODEL` 已弃用，由 HAIKU 取代（如需仍可在下方自由区填写）。 */
export const CC_MODEL_FIELDS = [
  { key: 'ANTHROPIC_MODEL', label: 'ANTHROPIC_MODEL', hint: '主模型' },
  { key: 'ANTHROPIC_DEFAULT_FABLE_MODEL', label: '…DEFAULT_FABLE_MODEL', hint: 'fable 别名' },
  { key: 'ANTHROPIC_DEFAULT_OPUS_MODEL', label: '…DEFAULT_OPUS_MODEL', hint: 'opus 别名' },
  { key: 'ANTHROPIC_DEFAULT_SONNET_MODEL', label: '…DEFAULT_SONNET_MODEL', hint: 'sonnet 别名' },
  { key: 'ANTHROPIC_DEFAULT_HAIKU_MODEL', label: '…DEFAULT_HAIKU_MODEL', hint: 'haiku 别名（替代 SMALL_FAST_MODEL）' },
  { key: 'CLAUDE_CODE_SUBAGENT_MODEL', label: 'CLAUDE_CODE_SUBAGENT_MODEL', hint: '子 agent' }
] as const

/** 全部精选 key（连接 + 模型），用于把「其他环境变量」排除这些 */
export const CC_CURATED_KEYS = new Set<string>([
  CC_BASE_URL_KEY,
  CC_AUTH_TOKEN_KEY,
  ...CC_MODEL_FIELDS.map((f) => f.key)
])

/** 判断 key 是否为敏感凭证（与后端 is_secret_key 行为一致），前端据此掩码展示 */
export function isSecretKey(key: string): boolean {
  const k = (key || '').toUpperCase()
  return k.includes('TOKEN') || k.includes('SECRET') || k.includes('PASSWORD')
}

/** 把网关 listen 地址（如 `127.0.0.1:8000`）规范化为 Claude Code 的 base URL：
 * `http://127.0.0.1:8000`（不带 /v1 —— Claude Code 会自行拼接 /v1/messages） */
function gatewayBaseUrlForCc(listen: string): string {
  const addr = listen.trim()
  if (!addr) return ''
  let url =
    addr.startsWith('http://') || addr.startsWith('https://') ? addr : `http://${addr}`
  url = url.replace(/\/+$/, '')
  if (url.endsWith('/v1')) url = url.slice(0, -3)
  return url
}

export function useClaudeCodeConfig() {
  const { toast, alert: alertModal } = useDialog()

  const form = ref<CcForm>({ env: [] })
  const loading = ref(false)
  const saving = ref(false)
  const filePath = ref('')
  const fileExists = ref(false)
  /** 网关 base URL（http://{listen}，无 /v1），用于「一键对接」预填 */
  const gatewayBaseUrl = ref('')
  /** 网关 consumer key 列表，用于「一键对接」取首个 */
  const consumerKeys = ref<string[]>([])
  /** 网关 consumer 模型列表，作为模型字段下拉候选 */
  const consumerModels = ref<string[]>([])

  /** 正在明文编辑的 env key（敏感值默认掩码，点按后转明文输入） */
  const editingKeys = ref<Set<string>>(new Set())

  // ---------- env 条目读写 ----------
  function findEntry(key: string): CcEnvEntry | undefined {
    return form.value.env.find((e) => e.key === key)
  }
  function getEnv(key: string): string {
    return findEntry(key)?.value ?? ''
  }
  /** 设置某 key 的值：存在则就地改（保持引用稳定），不存在则追加 */
  function setEnv(key: string, value: string) {
    const e = findEntry(key)
    if (e) {
      e.value = value
    } else {
      form.value.env.push({ key, value, secret: isSecretKey(key) })
    }
  }

  /** 模型字段下拉候选：网关 consumer 模型 + 当前 6 个模型字段值兜底 */
  const modelSelectOptions = computed(() => {
    const opts: string[] = []
    const seen = new Set<string>()
    for (const m of consumerModels.value) {
      if (m && !seen.has(m)) {
        seen.add(m)
        opts.push(m)
      }
    }
    for (const f of CC_MODEL_FIELDS) {
      const v = getEnv(f.key)
      if (v && !seen.has(v)) {
        seen.add(v)
        opts.push(v)
      }
    }
    return opts
  })

  /** 未被精选字段占用的 env 条目（自由编辑区） */
  const customEntries = computed(() =>
    form.value.env.filter((e) => !CC_CURATED_KEYS.has(e.key))
  )

  /** 从完整路径中提取文件名（如 settings.json） */
  const fileBaseName = computed(() => {
    const p = filePath.value
    if (!p) return 'settings.json'
    const parts = p.replace(/\\/g, '/').split('/')
    return parts[parts.length - 1] || 'settings.json'
  })

  const envCount = computed(() => form.value.env.length)
  const customCount = computed(() => customEntries.value.length)

  async function load() {
    loading.value = true
    try {
      const res = await claudeCodeConfigLoad()
      filePath.value = res.path
      fileExists.value = res.exists
      // 同步读取网关 listen / consumer 信息，用于「一键对接」与模型下拉候选
      try {
        const cfg = await getConfig()
        gatewayBaseUrl.value = gatewayBaseUrlForCc(cfg.listen)
        consumerKeys.value = cfg.consumer.api_keys ?? []
        consumerModels.value = cfg.consumer.models ?? []
      } catch {
        gatewayBaseUrl.value = ''
        consumerKeys.value = []
        consumerModels.value = []
      }
      // 规范化：后端 secret 标记兜底（缺失时按 key 名推断）
      const env = Array.isArray(res.form?.env)
        ? res.form.env.map((e) => ({ ...e, secret: e.secret ?? isSecretKey(e.key) }))
        : []
      form.value = { env }
      editingKeys.value.clear()
    } catch (e) {
      await alertModal({ title: '读取失败', message: String(e) })
    } finally {
      loading.value = false
    }
  }

  async function save() {
    saving.value = true
    try {
      // 清洗：trim key、去空 key、去空值（空值条目不写入，与 opencode 置空删除语义一致）
      const cleanForm: CcForm = {
        env: form.value.env
          .map((e) => ({ key: e.key.trim(), value: e.value, secret: e.secret }))
          .filter((e) => e.key !== '' && e.value.trim() !== '')
      }
      await claudeCodeConfigSave(cleanForm)
      toast('已保存 · 已备份', 'success')
    } catch (e) {
      toast('保存失败: ' + String(e), 'error', 5000)
    } finally {
      saving.value = false
    }
  }

  // ---------- 敏感值掩码 / 明文编辑切换 ----------
  function startEdit(key: string) {
    editingKeys.value = new Set(editingKeys.value).add(key)
    editingKeys.value = new Set(editingKeys.value)
  }
  function endEdit(key: string) {
    const set = new Set(editingKeys.value)
    set.delete(key)
    editingKeys.value = set
  }
  function maskedValue(key: string): string {
    const v = getEnv(key)
    return v ? maskKey(v) : ''
  }

  // ---------- 自定义 env 增删 ----------
  function addCustomEntry() {
    form.value.env.push({ key: '', value: '', secret: false })
  }
  function removeEntry(entry: CcEnvEntry) {
    const i = form.value.env.indexOf(entry)
    if (i >= 0) form.value.env.splice(i, 1)
  }
  /** 自定义条目 key 变更时同步 secret 标记 */
  function onCustomKeyInput(entry: CcEnvEntry, raw: string) {
    entry.key = raw
    entry.secret = isSecretKey(raw)
  }

  // ---------- 一键对接本网关 ----------
  function linkToGateway() {
    if (!gatewayBaseUrl.value) {
      toast('网关未运行或未配置监听地址', 'error')
      return
    }
    if (!consumerKeys.value.length) {
      toast('未配置 consumer key，请先在「设置」页添加', 'error')
      return
    }
    setEnv(CC_BASE_URL_KEY, gatewayBaseUrl.value)
    setEnv(CC_AUTH_TOKEN_KEY, consumerKeys.value[0])
    toast('已填入网关地址与首个 consumer key，记得保存', 'success')
  }

  onMounted(load)

  return {
    // 状态
    form,
    loading,
    saving,
    filePath,
    fileExists,
    gatewayBaseUrl,
    editingKeys,
    // 计算属性
    modelSelectOptions,
    customEntries,
    fileBaseName,
    envCount,
    customCount,
    // 动作
    load,
    save,
    linkToGateway,
    // env 读写
    getEnv,
    setEnv,
    // 掩码 / 编辑
    startEdit,
    endEdit,
    maskedValue,
    // 自定义 env 增删
    addCustomEntry,
    removeEntry,
    onCustomKeyInput
  }
}
