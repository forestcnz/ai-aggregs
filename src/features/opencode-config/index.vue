<script setup lang="ts">
import { useOpencodeConfig, MODALITY_OPTIONS, npmSelectOptions } from './index'
import ModelCombobox from './ModelCombobox.vue'
import MultiCombobox from './MultiCombobox.vue'
import type { OcModel } from '../../api/commands'

const props = defineProps<{ version?: string | null }>()

const {
  form,
  loading,
  saving,
  filePath,
  fileExists,
  expandedProviders,
  expandedModels,
  modelSelectOptions,
  providerCount,
  modelTotalCount,
  fileBaseName,
  load,
  save,
  toggleProvider,
  toggleModel,
  editingKeys,
  startEditKey,
  endEditKey,
  maskedKey,
  addProvider,
  removeProvider,
  addModel,
  removeModel,
  toggleModality,
  modelFlags,
  isProviderDisabled,
  availableProviderIds,
  loadingProviderIds,
  refreshProviderIds,
  disabledProviders
} = useOpencodeConfig()

/** limit 字段的受控输入：空串视为删除 limit 对象 */
function onLimitInput(m: OcModel, field: 'context' | 'output', raw: string) {
  const trimmed = raw.trim()
  if (trimmed === '') {
    // 两个字段都空时移除整个 limit
    if (field === 'context') {
      if (!m.limit || !m.limit.output) m.limit = null
      else if (m.limit) m.limit.context = 0
    } else {
      if (!m.limit || !m.limit.context) m.limit = null
      else if (m.limit) m.limit.output = 0
    }
    return
  }
  const n = Number(trimmed)
  if (!Number.isNaN(n)) {
    if (!m.limit) m.limit = { context: 0, output: 0 }
    if (field === 'context') m.limit.context = n
    else m.limit.output = n
  }
}
</script>

<template>
  <div v-if="loading" class="oc-loading">加载中…</div>
  <div v-else-if="form" class="oc-page">
    <h1 class="page-title">
      OpenCode 配置
      <span v-if="props.version" class="file-tag" title="opencode 版本">v{{ props.version }}</span>
      <span class="file-tag">{{ fileBaseName }}</span>
    </h1>
    <p class="page-sub">编辑 OpenCode 工具配置 · 与本网关联动</p>

    <!-- 工具栏 -->
    <div class="oc-toolbar">
      <div class="oc-path">
        <span class="pth">{{ filePath || '~/.config/opencode/opencode.jsonc' }}</span>
        <span v-if="fileExists" class="meta">· 已存在</span>
        <span v-else class="meta">· 不存在（保存时新建）</span>
      </div>
      <div class="oc-actions">
        <button class="btn btn-secondary sm" @click="load">重新加载</button>
      </div>
    </div>

    <!-- 01 屏蔽 -->
    <div class="group">
      <h3>
        <span class="gn">01</span>屏蔽 Provider
        <span class="cnt">· 已屏蔽 {{ disabledProviders.length }} 个 · 候选 {{ availableProviderIds.length }} 个</span>
        <button
          class="btn btn-secondary sm oc-refresh"
          :disabled="loadingProviderIds"
          @click="refreshProviderIds"
        >
          {{ loadingProviderIds ? '获取中…' : '刷新候选' }}
        </button>
      </h3>
      <p class="oc-blocked-tip">
        候选列表由 <code>opencode models</code> 动态获取；被屏蔽的 provider 不会被 opencode 加载
        （对应顶层 <code>disabled_providers</code>）。可多选，也可手动输入任意 id 后回车添加。
      </p>
      <MultiCombobox
        v-model="disabledProviders"
        :options="availableProviderIds"
        placeholder="选择候选 provider，或输入任意 id（如 openai / gemini）后回车"
      />
    </div>

    <!-- 02 基础 -->
    <div class="group">
      <h3><span class="gn">02</span>基础</h3>
      <div class="row">
        <label>主模型 model</label>
        <ModelCombobox
          v-model="form.model"
          :options="modelSelectOptions"
          :loading="loadingProviderIds"
          placeholder="providerId/modelId（可下拉或手动输入）"
        />
      </div>
      <div class="row">
        <label>轻量模型 small_model</label>
        <ModelCombobox
          v-model="form.small_model"
          :options="modelSelectOptions"
          :loading="loadingProviderIds"
          placeholder="留空回退主模型（可下拉或手动输入）"
        />
      </div>
      <div class="row">
        <label>默认 agent</label>
        <input v-model="form.default_agent" class="inp" placeholder="build / plan / 自定义" />
      </div>
    </div>

    <!-- 02 Provider -->
    <div class="group">
      <h3>
        <span class="gn">03</span>Provider
        <span class="cnt">· {{ providerCount }} 个 · {{ modelTotalCount }} 个模型</span>
      </h3>

      <div v-if="!form.providers.length" class="oc-empty">暂无 provider，点击下方添加</div>

      <div class="oc-providers">
        <div
          v-for="(p, pi) in form.providers"
          :key="pi"
          class="pv-card"
          :class="{ expanded: expandedProviders.has(p), disabled: isProviderDisabled(p) }"
        >
          <div class="pv-head" @click="toggleProvider(p)">
            <span class="pv-chevron">
              <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
                <path d="M3 2l4 3-4 3" stroke="currentColor" stroke-width="1.3" stroke-linecap="square" />
              </svg>
            </span>
            <span class="pv-id">{{ p.id || '(未命名)' }}</span>
            <span v-if="isProviderDisabled(p)" class="pv-badge-disabled">已屏蔽</span>
            <span v-if="p.npm" class="pv-npm">{{ p.npm }}</span>
            <span class="pv-modelcount">{{ p.models.length }} models</span>
            <button class="btn btn-secondary sm" @click.stop="removeProvider(pi)">删除</button>
          </div>
          <div v-if="expandedProviders.has(p)" class="pv-body">
            <div class="pv-fields">
              <div class="mf">
                <label>id <span class="req">*</span></label>
                <input v-model="p.id" class="inp" placeholder="如 Local-oai" />
              </div>
              <div class="mf">
                <label>name</label>
                <input v-model="p.name" class="inp" placeholder="显示名" />
              </div>
            </div>
            <div class="pv-fields full">
              <div class="mf">
                <label>npm</label>
                <select v-model="p.npm">
                  <option :value="null">— 未设置 —</option>
                  <option v-for="n in npmSelectOptions(p.npm)" :key="n" :value="n">{{ n }}</option>
                </select>
              </div>
            </div>
            <div class="pv-fields full">
              <div class="mf">
                <label>baseURL</label>
                <input v-model="p.options.baseURL" class="inp mono" placeholder="http://127.0.0.1:8000/v1" />
              </div>
            </div>
            <div class="pv-fields full">
              <div class="mf">
                <label>apiKey</label>
                <div class="key-field">
                  <template v-if="editingKeys.has(p) || !p.options.apiKey">
                    <input
                      v-model="p.options.apiKey"
                      class="inp mono"
                      placeholder="留空将写入空值"
                      autofocus
                      @blur="endEditKey(p)"
                      @keydown.enter.prevent="($event.target as HTMLInputElement).blur()"
                    />
                  </template>
                  <template v-else>
                    <span class="key-display" @click="startEditKey(p)">
                      {{ maskedKey(p) }}
                    </span>
                  </template>
                </div>
              </div>
            </div>

            <!-- models 子列表 -->
            <div class="pv-models-head">
              <span class="lbl">Models <span class="ct">{{ p.models.length }} 个</span></span>
              <button class="btn btn-secondary sm" @click="addModel(p)">+ 添加模型</button>
            </div>

            <div v-if="!p.models.length" class="oc-empty sm">暂无模型</div>

            <div
              v-for="(m, mi) in p.models"
              :key="mi"
              class="md-row"
              :class="{ expanded: expandedModels.has(m) }"
            >
              <div class="md-row-head" @click="toggleModel(m)">
                <span class="md-chevron">
                  <svg width="9" height="9" viewBox="0 0 10 10" fill="none">
                    <path d="M3 2l4 3-4 3" stroke="currentColor" stroke-width="1.3" stroke-linecap="square" />
                  </svg>
                </span>
                <span class="md-id">{{ m.id || '(未命名)' }}</span>
                <span v-if="m.name" class="md-name">{{ m.name }}</span>
                <span class="md-flags">
                  <span
                    v-for="f in modelFlags(m)"
                    :key="f.label"
                    class="md-flag on"
                  >{{ f.label }}</span>
                  <span v-if="modelFlags(m).length === 0" class="md-flag">无标志</span>
                </span>
                <button class="btn btn-secondary sm md-del" @click.stop="removeModel(p, mi)">×</button>
              </div>
              <div v-if="expandedModels.has(m)" class="md-body">
                <div class="md-grid">
                  <div class="mf">
                    <label>id <span class="req">*</span></label>
                    <input v-model="m.id" class="inp" placeholder="如 glm-5.2" />
                  </div>
                  <div class="mf">
                    <label>name</label>
                    <input v-model="m.name" class="inp" placeholder="显示名" />
                  </div>
                </div>
                <!-- 开关组 -->
                <div class="md-toggles">
                  <label class="toggle mini">
                    <input v-model="m.attachment" type="checkbox" />
                    <span class="slider"></span>
                  </label>
                  <span class="tg-label">attachment</span>
                  <label class="toggle mini">
                    <input v-model="m.reasoning" type="checkbox" />
                    <span class="slider"></span>
                  </label>
                  <span class="tg-label">reasoning</span>
                  <label class="toggle mini">
                    <input v-model="m.tool_call" type="checkbox" />
                    <span class="slider"></span>
                  </label>
                  <span class="tg-label">tool_call</span>
                </div>
                <!-- modalities 多选 -->
                <div class="md-mods">
                  <div class="md-mods-lab">Modalities</div>
                  <div class="md-mods-group">
                    <span class="md-mods-sublab">input</span>
                    <div class="mod-tags">
                      <span
                        v-for="opt in MODALITY_OPTIONS.input"
                        :key="opt"
                        class="mod-tag"
                        :class="{ on: m.modalities.input.includes(opt) }"
                        @click="toggleModality(m, 'input', opt)"
                      >
                        <span class="chk"></span>{{ opt }}
                      </span>
                    </div>
                  </div>
                  <div class="md-mods-group">
                    <span class="md-mods-sublab">output</span>
                    <div class="mod-tags">
                      <span
                        v-for="opt in MODALITY_OPTIONS.output"
                        :key="opt"
                        class="mod-tag"
                        :class="{ on: m.modalities.output.includes(opt) }"
                        @click="toggleModality(m, 'output', opt)"
                      >
                        <span class="chk"></span>{{ opt }}
                      </span>
                    </div>
                  </div>
                </div>
                <!-- limit -->
                <div class="md-limit">
                  <div class="mf">
                    <label>limit.context</label>
                    <input
                      class="inp mono"
                      type="number"
                      :value="m.limit?.context ?? ''"
                      placeholder="1000000"
                      @input="onLimitInput(m, 'context', ($event.target as HTMLInputElement).value)"
                    />
                  </div>
                  <div class="mf">
                    <label>limit.output</label>
                    <input
                      class="inp mono"
                      type="number"
                      :value="m.limit?.output ?? ''"
                      placeholder="131072"
                      @input="onLimitInput(m, 'output', ($event.target as HTMLInputElement).value)"
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div class="oc-add">
        <button class="btn btn-secondary sm" @click="addProvider">+ 添加 Provider</button>
        <span class="hint">provider id 即 model 字段前缀（如 Local-oai/glm-5.2）</span>
      </div>
    </div>

    <!-- 保存栏 -->
    <div class="actions">
      <span class="save-hint">按 key 合并写入 · mcp / permission / agent 等未表单字段原样保留 · 保存前自动备份</span>
      <button class="btn btn-primary" :disabled="saving" @click="save">
        {{ saving ? '保存中…' : `保存到 ${fileBaseName}` }}
      </button>
    </div>
  </div>
</template>

<style src="./index.css" scoped></style>
