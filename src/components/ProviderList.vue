<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import {
  getConfig, saveConfig, runtimeStatus, toggleProvider, toggleKey,
  normalizeKey,
  type Config, type ProviderConfig, type ProviderRuntime,
} from '../api/commands'

defineProps<{ gatewayRunning: boolean }>()

const config = ref<Config | null>(null)
const runtimeMap = ref<Map<string, ProviderRuntime>>(new Map())
const loading = ref(false)
const saving = ref(false)
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
    name: '', protocol: 'chat', base_url: '', api_keys: [],
    models: [], timeout_secs: 300, max_retries: 2,
    extra_headers: {}, enabled: true, reasoning_effort: null,
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
    runtimeMap.value = new Map(rt.map(p => [p.name, p]))
    lastRuntimeAt.value = Date.now()
  } catch { /* ignore */ }
}

function showMsg(text: string) {
  msg.value = text
  setTimeout(() => { msg.value = '' }, 3000)
}

async function save() {
  if (!config.value) return
  saving.value = true
  try {
    await saveConfig(config.value)
    await refreshRuntime()
    showMsg('已保存')
  } catch (e) { showMsg('保存失败: ' + String(e)) }
  finally { saving.value = false }
}

// ---- 卡片操作 ----

async function onToggleProvider(idx: number, enabled: boolean) {
  if (!config.value) return
  loading.value = true
  try {
    await toggleProvider(config.value.providers[idx].name, enabled)
    config.value.providers[idx].enabled = enabled
    await refreshRuntime()
  } catch (e) { showMsg(String(e)) }
  finally { loading.value = false }
}

// 切换单个 key 的启用状态
async function onToggleKey(providerName: string, keyIdx: number, enabled: boolean) {
  loading.value = true
  try {
    await toggleKey(providerName, keyIdx, enabled)
    await refreshRuntime()
  } catch (e) { showMsg(String(e)) }
  finally { loading.value = false }
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

function submitModal() {
  const p = editingProvider.value
  if (!p.name.trim() || !p.base_url.trim()) { showMsg('名称和 Base URL 不能为空'); return }
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
  if (editingIdx.value < 0 || !config.value) { modalMode.value = null; return }
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
function modalRemoveModel(i: number) { editingProvider.value.models.splice(i, 1) }

function modalAddKey() {
  const v = keyInput.value.trim()
  if (!v) return
  editingProvider.value.api_keys.push({ key: v, enabled: true })
  keyInput.value = ''
}
function modalRemoveKey(i: number) { editingProvider.value.api_keys.splice(i, 1) }



// ---- 辅助 ----

function getRuntime(name: string) { return runtimeMap.value.get(name) }

// 按 idx 从 runtime 中取某个 key 的运行时状态（用于黑名单/禁用）
function keyRuntime(name: string, idx: number) {
  return getRuntime(name)?.keys.find(k => k.idx === idx)
}

function maskKey(key: string): string {
  if (key.length <= 12) return key.slice(0, 4) + '**'
  return key.slice(0, 6) + '**' + key.slice(-6)
}

// 按协议返回图标 emoji
function iconFor(protocol: string): string {
  switch (protocol) {
    case 'anthropic': return '🧠'
    case 'responses': return '🔄'
    default: return '💬'
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
})
onUnmounted(() => {
  if (timer) clearInterval(timer)
})
</script>

<template>
  <div v-if="config">
    <!-- 标题栏 -->
    <div class="header-row">
      <div>
        <h1 class="page-title">提供商管理</h1>
        <p class="page-sub">管理上游 API 提供商及密钥</p>
      </div>
      <div class="header-actions">
        <span v-if="msg" class="save-msg" :class="{ ok: msg.includes('已保存') }">{{ msg }}</span>
        <button class="btn btn-primary sm" @click="openAdd">+ 添加提供商</button>
      </div>
    </div>

    <!-- 卡片网格 -->
    <div class="card-grid">
      <div
        v-for="(p, idx) in config.providers"
        :key="idx"
        class="provider-card"
        :class="{ off: !p.enabled }"
      >
        <!-- 卡片头部（双击编辑） -->
        <div class="provider-card-header" @dblclick="openEdit(idx)">
          <div class="provider-card-left">
            <div class="provider-icon">{{ iconFor(p.protocol) }}</div>
            <div class="provider-name">
              <span class="name-text">{{ p.name }}</span>
              <span class="protocol-tag">{{ p.protocol }}</span>
            </div>
          </div>
          <div class="provider-card-right">
            <label class="toggle" @click.stop>
              <input type="checkbox" :checked="p.enabled" :disabled="loading"
                @change="onToggleProvider(idx, ($event.target as HTMLInputElement).checked)" />
              <span class="slider"></span>
            </label>
          </div>
        </div>

        <!-- 详情区（始终展开） -->
        <div class="provider-card-body">
          <div class="provider-field">
            <span class="f-label">Base URL</span>
            <span class="f-value mono">{{ p.base_url }}</span>
          </div>
          <div class="provider-field">
            <span class="f-label">Models</span>
            <div class="models-tags f-value">
              <span v-for="m in p.models" :key="m">{{ m }}</span>
              <span v-if="!p.models.length" class="muted">（空）</span>
            </div>
          </div>
          <div class="provider-field">
            <span class="f-label">Reasoning</span>
            <span class="f-value">{{ p.reasoning_effort || '无' }}</span>
          </div>
          <div class="provider-field">
            <span class="f-label">超时</span>
            <span class="f-value">{{ p.timeout_secs }}s</span>
          </div>

          <!-- API Keys 列表（基于配置，状态取自运行时） -->
          <div class="keys-section">
            <div class="keys-header">
              <span>API Keys（{{ p.api_keys.length }}）</span>
            </div>
            <template v-if="p.api_keys.length">
              <div v-for="(entry, ki) in p.api_keys" :key="ki" class="key-row">
                <label class="toggle" @click.stop>
                  <input type="checkbox" :checked="normalizeKey(entry).enabled" :disabled="loading"
                    @change="onToggleKey(p.name, ki, ($event.target as HTMLInputElement).checked)" />
                  <span class="slider"></span>
                </label>
                <span class="key-value">{{ maskKey(normalizeKey(entry).key) }}</span>
                <span v-if="keyRuntime(p.name, ki)?.blacklisted" class="key-status blacklisted">
                  {{ fmtTime(keyReleaseTime(keyRuntime(p.name, ki)?.blacklist_remaining_secs)!) }}
                </span>
                <span v-else-if="!normalizeKey(entry).enabled" class="key-status disabled">已禁用</span>
                <span v-else-if="getRuntime(p.name)" class="key-status ok">正常</span>
              </div>
            </template>
            <div v-else class="muted">未配置 key</div>
          </div>
        </div>
      </div>

      <!-- 空状态 -->
      <div v-if="config.providers.length === 0" class="empty">
        暂无提供商，点击右上角"添加提供商"开始配置
      </div>
    </div>

    <!-- 添加 / 编辑 弹窗 -->
    <Teleport to="body">
      <div v-if="modalMode" class="modal-overlay" @click.self="closeModal">
        <div class="modal">
          <div class="modal-header">
            <h2>{{ modalMode === 'add' ? '添加提供商' : '编辑提供商' }}</h2>
            <button class="modal-close" @click="closeModal">✕</button>
          </div>

          <div class="modal-body">
            <div class="mf">
              <label>名称</label>
              <input v-model="editingProvider.name" class="f-input" placeholder="如 glm" />
            </div>
            <div class="mf-row">
              <!-- 协议选择 — 分段控件 -->
              <div class="mf">
                <label>协议</label>
                <div class="seg">
                  <input type="radio" id="proto-chat" value="chat" v-model="editingProvider.protocol">
                  <label for="proto-chat">chat</label>
                  <input type="radio" id="proto-resp" value="responses" v-model="editingProvider.protocol">
                  <label for="proto-resp">responses</label>
                  <input type="radio" id="proto-anth" value="anthropic" v-model="editingProvider.protocol">
                  <label for="proto-anth">anthropic</label>
                </div>
              </div>
              <div class="mf">
                <label>思考强度</label>
                <select v-model="editingProvider.reasoning_effort" class="f-select">
                  <option :value="null">无</option>
                  <option value="max">max</option>
                  <option value="xhigh">xhigh</option>
                  <option value="high">high</option>
                  <option value="medium">medium</option>
                  <option value="low">low</option>
                  <option value="minimal">minimal</option>
                </select>
              </div>
            </div>
            <div class="mf">
              <label>Base URL</label>
              <input v-model="editingProvider.base_url" class="f-input" placeholder="https://api.example.com/v1" />
            </div>

            <!-- API Keys（先填 key 再填模型） -->
            <div class="mf">
              <label>API Keys</label>
              <div class="chip-input">
                <span v-for="(k, ki) in editingProvider.api_keys" :key="ki" class="chip">
                  <span class="chip-text">{{ maskKey(normalizeKey(k).key) }}</span>
                  <button class="chip-x" @click="modalRemoveKey(ki)">×</button>
                </span>
                <input v-model="keyInput" class="chip-field" :placeholder="editingProvider.api_keys.length ? '' : 'API Key，回车添加'" @keydown.enter.prevent="modalAddKey" />
              </div>
            </div>

            <!-- 模型 -->
            <div class="mf">
              <label>模型</label>
              <div class="chip-input">
                <span v-for="(m, mi) in editingProvider.models" :key="mi" class="chip">
                  <span class="chip-text">{{ m }}</span>
                  <button class="chip-x" @click="modalRemoveModel(mi)">×</button>
                </span>
                <input v-model="modelInput" class="chip-field" :placeholder="editingProvider.models.length ? '' : '模型名，回车添加'" @keydown.enter.prevent="modalAddModel" />
              </div>
            </div>

            <div class="mf-row">
              <div class="mf">
                <label>超时（秒）</label>
                <input v-model.number="editingProvider.timeout_secs" type="number" class="f-input" />
              </div>
              <div class="mf">
                <label>重试次数</label>
                <input v-model.number="editingProvider.max_retries" type="number" class="f-input" />
              </div>
            </div>
          </div>

          <div class="modal-footer">
            <button v-if="modalMode === 'edit'" class="btn btn-secondary danger-btn" @click="deleteFromModal">删除</button>
            <div class="footer-right">
              <button class="btn btn-secondary" @click="closeModal">取消</button>
              <button class="btn btn-primary" @click="submitModal">{{ modalMode === 'add' ? '创建' : '保存' }}</button>
            </div>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.header-row { display: flex; align-items: flex-start; justify-content: space-between; margin-bottom: 20px; }
.page-title { font-size: 20px; font-weight: 600; color: var(--text-strong); letter-spacing: -.01em; }
.page-sub { font-size: 12px; color: var(--text-weak); margin-top: 2px; }
.header-actions { display: flex; align-items: center; gap: 12px; }
.save-msg { font-size: 12px; color: var(--text-weak); white-space: nowrap; }
.save-msg.ok { color: var(--green); }
.btn.sm { padding: 5px 10px; font-size: 12px; }
.btn.xs { padding: 3px 8px; font-size: 11px; }

/* ---- 卡片网格 ---- */
.card-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.provider-card {
  background: var(--bg); border: 1px solid var(--border-weak); border-radius: var(--r-md);
  overflow: hidden; display: flex; flex-direction: column; transition: var(--transition);
}
.provider-card:hover { border-color: var(--border); }
.provider-card.off { opacity: .45; }

/* 协议内联小标签（名称旁） */
.protocol-tag {
  font-size: 10px; font-weight: 400; color: var(--text-weak);
  border: 1px solid var(--border-weak); padding: 1px 6px; border-radius: var(--r-sm);
  margin-left: 8px;
}

/* 卡片头部 */
.provider-card-header {
  display: flex; align-items: center; justify-content: space-between;
  padding: 12px 15px; cursor: pointer; user-select: none;
}
.provider-card-left { display: flex; align-items: center; gap: 10px; min-width: 0; }
.provider-icon {
  width: 28px; height: 28px; border-radius: var(--r-sm); border: 1px solid var(--border-weak);
  display: flex; align-items: center; justify-content: center;
  font-size: 13px; flex-shrink: 0;
}
.provider-name {
  display: flex; align-items: center; min-width: 0;
  font-size: 13px; gap: 0;
}
.provider-name .name-text {
  font-weight: 500; color: var(--text-strong);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.provider-card-right { display: flex; align-items: center; gap: 10px; flex-shrink: 0; }

/* 展开详情区 */
.provider-card-body {
  padding: 12px 15px; border-top: 1px solid var(--border-weak);
  display: flex; flex-direction: column; gap: 9px;
}
.provider-field {
  display: grid; grid-template-columns: 80px 1fr; gap: 7px; font-size: 12px;
  align-items: start;
}
.provider-field .f-label { color: var(--text-weak); font-size: 11px; }
.provider-field .f-value { color: var(--text); word-break: break-all; }
.provider-field .f-value.mono { font-size: 11px; }
.models-tags { display: flex; flex-wrap: wrap; gap: 4px; }
.models-tags span {
  font-size: 10px; padding: 2px 7px; border-radius: var(--r-sm);
  border: 1px solid var(--border-weak); color: var(--text);
}
.muted { color: var(--text-weak); font-style: italic; font-size: 11px; }

/* Key 列表 */
.keys-section { margin-top: 2px; padding-top: 10px; border-top: 1px solid var(--border-weak); }
.keys-header {
  display: flex; align-items: center; justify-content: space-between;
  margin-bottom: 7px;
}
.keys-header span { font-size: 10px; color: var(--text-weak); }
.key-row {
  display: flex; align-items: center; gap: 10px;
  padding: 5px 9px; border-radius: var(--r-sm);
  border: 1px solid var(--border-weak); margin-bottom: 4px;
}
.key-row .key-value { flex: 1; font-size: 11px; color: var(--text); }
.key-status { font-size: 9px; padding: 1px 7px; border-radius: var(--r-sm); white-space: nowrap; font-weight: 500; border: 1px solid; }
.key-status.ok { border-color: var(--green); color: var(--green); }
.key-status.blacklisted { border-color: var(--red); color: var(--red); }
.key-status.disabled { border-color: var(--border-weak); color: var(--text-weak); }

.empty {
  grid-column: 1 / -1; text-align: center; padding: 60px;
  color: var(--text-weak); font-size: 13px;
}

/* ---- 弹窗 ---- */
.modal-overlay {
  position: fixed; inset: 0; z-index: 1000;
  background: rgba(31,30,30,.16); backdrop-filter: blur(1.5px);
  display: flex; align-items: center; justify-content: center;
}
.modal {
  background: var(--bg); border: 1px solid var(--text-strong); border-radius: var(--r-md);
  box-shadow: 0 20px 60px rgba(0,0,0,.18);
  width: 500px; max-width: 90vw; max-height: 90vh; overflow-y: auto;
}
.modal-header {
  display: flex; align-items: center; justify-content: space-between;
  padding: 16px 22px; border-bottom: 1px solid var(--border-weak);
  position: sticky; top: 0; background: var(--bg); z-index: 1;
}
.modal-header h2 { font-size: 15px; font-weight: 600; color: var(--text-strong); }
.modal-close {
  background: var(--bg); border: 1px solid var(--border-weak); width: 26px; height: 26px;
  border-radius: var(--r-sm); font-size: 13px; cursor: pointer;
  color: var(--text-weak); line-height: 1; transition: var(--transition);
  display: flex; align-items: center; justify-content: center;
}
.modal-close:hover { border-color: var(--text-strong); color: var(--text-strong); }
.modal-body { padding: 18px 22px; display: flex; flex-direction: column; gap: 14px; }
.mf { display: flex; flex-direction: column; gap: 5px; flex: 1; }
.mf label { font-size: 12px; font-weight: 500; color: var(--text-strong); }
.mf-row { display: flex; gap: 12px; }
.mf .f-input, .mf .f-select { width: 100%; max-width: none; }
.modal-footer {
  display: flex; align-items: center; justify-content: space-between;
  padding: 13px 22px; border-top: 1px solid var(--border-weak);
  position: sticky; bottom: 0; background: var(--bg);
}
.footer-right { display: flex; gap: 8px; margin-left: auto; }
.danger-btn { color: var(--red); border-color: var(--border-weak); }
.danger-btn:hover { background: var(--red); color: var(--text-inverted); border-color: var(--red); }

/* ---- 协议分段控件 ---- */
.seg { display: flex; border: 1px solid var(--border-weak); border-radius: var(--r-sm); overflow: hidden; background: var(--bg); }
.seg input { position: absolute; opacity: 0; width: 0; height: 0; }
.seg > label {
  flex: 1; text-align: center; padding: 7px 5px; font-size: 11px; font-weight: 500;
  color: var(--text-weak); cursor: pointer; transition: var(--transition);
  border-right: 1px solid var(--border-weak);
}
.seg > label:last-child { border-right: none; }
.seg > label:hover { background: var(--bg-weak); color: var(--text-strong); }
.seg input:checked + label { background: var(--bg-strong); color: var(--text-inverted); border-color: var(--bg-strong); }

/* ---- 输入框 ---- */
.f-input {
  background: var(--bg-weak); border: 1px solid var(--border-weak); border-radius: var(--r-md);
  padding: 8px 12px; color: var(--text-strong); font-size: 12px; outline: none;
  font-family: inherit; transition: var(--transition); width: 100%; max-width: 400px;
}
.f-input:focus { background: var(--bg-interactive-weaker); border-color: var(--text-strong); box-shadow: 0 0 0 3px var(--bg-interactive); }
.f-select {
  background: var(--bg-weak); border: 1px solid var(--border-weak); border-radius: var(--r-md);
  padding: 8px 12px; color: var(--text-strong); font-size: 12px; outline: none; cursor: pointer;
  font-family: inherit; width: 100%; transition: var(--transition);
}
.f-select:focus { border-color: var(--text-strong); box-shadow: 0 0 0 3px var(--bg-interactive); }

/* ---- Chip 输入框 ---- */
.chip-input {
  display: flex; flex-wrap: wrap; align-items: center; gap: 4px;
  background: var(--bg-weak); border: 1px solid var(--border-weak);
  border-radius: var(--r-md); padding: 5px 7px; cursor: text;
  min-height: 34px; transition: var(--transition);
}
.chip-input:focus-within { background: var(--bg-interactive-weaker); border-color: var(--text-strong); box-shadow: 0 0 0 3px var(--bg-interactive); }
.chip {
  display: inline-flex; align-items: center; gap: 3px;
  background: var(--bg); border: 1px solid var(--border-weak);
  border-radius: var(--r-sm); padding: 1px 2px 1px 7px; font-size: 11px;
  white-space: nowrap; cursor: pointer; user-select: none;
}
.chip:hover { border-color: var(--border); }
.chip-text { color: var(--text); font-size: 10px; }
.chip-x {
  background: none; border: none; color: var(--text-weak); cursor: pointer;
  font-size: 12px; line-height: 1; padding: 0 2px; transition: var(--transition);
}
.chip-x:hover { color: var(--red); }
.chip-field {
  flex: 1; min-width: 130px; border: none; outline: none; background: transparent;
  color: var(--text-strong); font-size: 12px; padding: 2px 0; font-family: inherit;
}
</style>
