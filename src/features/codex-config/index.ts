import { ref, computed, onMounted } from 'vue'
import {
  codexConfigLoad,
  codexConfigSave,
  getConfig,
  maskKey,
  gatewayV1Url,
  fileBaseName as fileBaseNameFn,
  type CodexForm,
  type CodexProvider
} from '../../api/commands'
import { useDialog } from '../../composables/useDialog'

/** 受管 provider 的默认 id（同时作为顶层 model_provider 的值） */
const DEFAULT_PROVIDER_ID = 'aggregs'

function emptyForm(): CodexForm {
  return {
    model: null,
    provider: {
      id: DEFAULT_PROVIDER_ID,
      name: null,
      base_url: null,
      experimental_bearer_token: null
    },
    loaded_provider_id: null,
    enable_model_catalog: false,
    catalog_models: []
  }
}

export function useCodexConfig() {
  const { toast, alert: alertModal } = useDialog()

  const form = ref<CodexForm>(emptyForm())
  const loading = ref(false)
  const saving = ref(false)
  const filePath = ref('')
  const fileExists = ref(false)
  const hasComments = ref(false)
  /** 网关 base URL（http://{listen}/v1），用于「一键对接」预填 */
  const gatewayBaseUrl = ref('')
  /** 网关 consumer key 列表，用于「一键对接」取首个 */
  const consumerKeys = ref<string[]>([])
  /** 网关 consumer 模型列表，作为 model 字段下拉候选 */
  const consumerModels = ref<string[]>([])

  /** token 是否处于明文编辑态（有值且非编辑态时掩码展示） */
  const editingToken = ref(false)

  /** model 字段下拉候选：网关 consumer 模型 + 当前值兜底 */
  const modelSelectOptions = computed(() => {
    const opts: string[] = []
    const seen = new Set<string>()
    for (const m of consumerModels.value) {
      if (m && !seen.has(m)) {
        seen.add(m)
        opts.push(m)
      }
    }
    const cur = form.value.model
    if (cur && !seen.has(cur)) {
      seen.add(cur)
      opts.push(cur)
    }
    return opts
  })

  /** 从完整路径中提取文件名（如 config.toml） */
  const fileBaseName = computed(() => fileBaseNameFn(filePath.value, 'config.toml'))

  async function load() {
    loading.value = true
    try {
      const res = await codexConfigLoad()
      filePath.value = res.path
      fileExists.value = res.exists
      hasComments.value = res.has_comments
      form.value = res.form
      // catalog_models/enable 可能因 skip_serializing 被省略，规整为数组/布尔
      form.value.catalog_models = res.form.catalog_models ?? []
      form.value.enable_model_catalog = res.form.enable_model_catalog ?? false
      editingToken.value = false
      // 同步读取网关 listen / consumer 信息，用于「一键对接」与模型下拉候选
      try {
        const cfg = await getConfig()
        gatewayBaseUrl.value = gatewayV1Url(cfg.listen)
        consumerKeys.value = cfg.consumer.api_keys ?? []
        consumerModels.value = cfg.consumer.models ?? []
      } catch {
        gatewayBaseUrl.value = ''
        consumerKeys.value = []
        consumerModels.value = []
      }
    } catch (e) {
      await alertModal({ title: '读取失败', message: String(e) })
    } finally {
      loading.value = false
    }
  }

  async function save() {
    saving.value = true
    try {
      // 清洗：trim 受管字段；catalog_models 去空去重；空值交后端处理
      const p = form.value.provider
      const catalogModels = Array.from(
        new Set(
          (form.value.catalog_models ?? [])
            .map((s) => s.trim())
            .filter((s) => s.length > 0)
        )
      )
      const enableCatalog = form.value.enable_model_catalog ?? false
      const cleanForm: CodexForm = {
        model: form.value.model?.trim() ? form.value.model!.trim() : null,
        provider: {
          id: p.id.trim(),
          name: p.name?.trim() ? p.name!.trim() : null,
          base_url: p.base_url?.trim() ? p.base_url!.trim() : null,
          experimental_bearer_token: p.experimental_bearer_token?.trim()
            ? p.experimental_bearer_token!.trim()
            : null
        },
        loaded_provider_id: form.value.loaded_provider_id ?? null,
        enable_model_catalog: enableCatalog,
        catalog_models: catalogModels
      }
      const result = await codexConfigSave(cleanForm)
      // 同步回显（id 可能被 trim，loaded_provider_id 跟进当前 id）
      form.value = {
        ...cleanForm,
        loaded_provider_id: cleanForm.provider.id ? cleanForm.provider.id : null
      }
      // 按模型目录结果 toast
      if (enableCatalog) {
        if (result.catalog_ok) {
          toast(`已保存 · 模型目录已生成（${result.catalog_count} 个）`, 'success')
        } else {
          toast(`已保存 · ⚠ 模型目录未生成：${result.catalog_error ?? '未知原因'}`, 'error', 6000)
        }
      } else {
        toast('已保存 · 已备份', 'success')
      }
    } catch (e) {
      toast('保存失败: ' + String(e), 'error', 5000)
    } finally {
      saving.value = false
    }
  }

  // ---------- bearer token 掩码 / 明文编辑切换 ----------
  function startEditToken() {
    editingToken.value = true
  }
  function endEditToken() {
    editingToken.value = false
  }
  function setToken(v: string) {
    form.value.provider.experimental_bearer_token = v
  }
  function maskedToken(): string {
    const v = form.value.provider.experimental_bearer_token
    return v ? maskKey(v) : ''
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
    const p: CodexProvider = {
      id: form.value.provider.id.trim() || DEFAULT_PROVIDER_ID,
      name: form.value.provider.name,
      base_url: gatewayBaseUrl.value,
      experimental_bearer_token: consumerKeys.value[0]
    }
    form.value.provider = p
    // model 为空时取首个 consumer 模型兜底
    if (!form.value.model && consumerModels.value.length) {
      form.value.model = consumerModels.value[0]
    }
    // 顺带启用模型目录，并用网关 consumer 模型预填清单（仅当清单为空，可编辑）
    form.value.enable_model_catalog = true
    if (!(form.value.catalog_models ?? []).length && consumerModels.value.length) {
      form.value.catalog_models = [...consumerModels.value]
    }
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
    hasComments,
    editingToken,
    consumerModels,
    // 计算属性
    modelSelectOptions,
    fileBaseName,
    // 动作
    load,
    save,
    linkToGateway,
    // token 掩码 / 编辑
    startEditToken,
    endEditToken,
    setToken,
    maskedToken
  }
}
