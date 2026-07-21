<script setup lang="ts">
import { useCodexConfig } from './index'
import ModelCombobox from '../../components/ModelCombobox.vue'
import MultiCombobox from '../../components/MultiCombobox.vue'

defineProps<{ version?: string | null }>()

const {
  form,
  loading,
  saving,
  filePath,
  fileExists,
  hasComments,
  editingToken,
  consumerModels,
  modelSelectOptions,
  fileBaseName,
  load,
  save,
  linkToGateway,
  startEditToken,
  endEditToken,
  setToken,
  maskedToken
} = useCodexConfig()
</script>

<template>
  <div v-if="loading" class="oc-loading">加载中…</div>
  <div v-else class="oc-page">
    <h1 class="page-title">
      Codex 配置
      <span class="beta-badge">beta</span>
      <span v-if="version" class="file-tag" title="codex 版本">v{{ version }}</span>
      <span class="file-tag">{{ fileBaseName }}</span>
    </h1>
    <p class="page-sub">
      编辑 Codex 的 <code>~/.codex/config.toml</code> · 把它指向本网关（Responses 协议） · 其余字段原样保留
    </p>

    <!-- 工具栏 -->
    <div class="oc-toolbar">
      <div class="oc-path">
        <span class="pth">{{ filePath || '~/.codex/config.toml' }}</span>
        <span v-if="fileExists" class="meta">· 已存在</span>
        <span v-else class="meta">· 不存在（保存时新建）</span>
      </div>
      <div class="oc-actions">
        <button class="btn btn-secondary sm" @click="linkToGateway">一键对接网关</button>
        <button class="btn btn-secondary sm" @click="load">重新加载</button>
      </div>
    </div>

    <!-- 注释丢失提示（TOML # 注释在重新序列化后会丢失） -->
    <div v-if="hasComments" class="comment-banner">
      <span class="ic">⚠</span>
      <span
        >当前文件含 <b>TOML 注释</b>。保存（重新序列化）后注释会丢失，已自动备份为带时间戳的
        <b>.bak</b>。</span
      >
    </div>

    <!-- 01 连接 -->
    <div class="group">
      <h3>
        <span class="gn">01</span>连接
        <span class="cnt">· 指向本网关（Responses 协议）</span>
      </h3>
      <p class="oc-blocked-tip">
        <code>base_url</code> 指到网关 <code>/v1</code>，Codex 走 Responses 协议（向
        <code>{base_url}/responses</code> 发请求，对应网关 <code>/v1/responses</code>）。
        <code>experimental_bearer_token</code> = 网关 consumer key，直接写入开箱即用（与
        <code>env_key</code> / <code>requires_openai_auth</code> 互斥，故不写后者）。点击工具栏「一键对接网关」可同时填入地址与首个 consumer key。
      </p>
      <div class="row">
        <label>provider id</label>
        <input
          v-model="form.provider.id"
          class="inp"
          placeholder="如 aggregs（即 model_providers 的 key 与 model_provider 的值）"
        />
      </div>
      <div class="row">
        <label>name</label>
        <input v-model="form.provider.name" class="inp" placeholder="显示名（可留空）" />
      </div>
      <div class="row">
        <label>base URL</label>
        <input
          v-model="form.provider.base_url"
          class="inp mono"
          placeholder="http://127.0.0.1:8000/v1"
        />
      </div>
      <div class="row">
        <label>bearer token</label>
        <div class="key-field">
          <template
            v-if="form.provider.experimental_bearer_token && !editingToken"
          >
            <span class="key-display" @click="startEditToken">
              {{ maskedToken() }}
            </span>
          </template>
          <template v-else>
            <input
              class="inp mono"
              :value="form.provider.experimental_bearer_token ?? ''"
              placeholder="experimental_bearer_token（网关 consumer key）"
              autofocus
              @input="setToken(($event.target as HTMLInputElement).value)"
              @blur="endEditToken"
            />
          </template>
        </div>
      </div>
    </div>

    <!-- 02 模型 -->
    <div class="group">
      <h3>
        <span class="gn">02</span>模型
        <span class="cnt">· 候选来自网关 consumer models · 网关后任意模型名原样透传</span>
      </h3>
      <div class="row">
        <label>model</label>
        <ModelCombobox
          :model-value="form.model"
          :options="modelSelectOptions"
          placeholder="留空回退 Codex 默认（可下拉或手动输入）"
          @update:model-value="(v) => (form.model = v ?? null)"
        />
      </div>
    </div>

    <!-- 03 模型目录 -->
    <div class="group">
      <h3>
        <span class="gn">03</span>模型目录
        <span class="cnt">· 让 /model 列出这些模型</span>
      </h3>
      <p class="oc-blocked-tip">
        开启后保存时，从下方清单克隆 Codex 内置模板生成 <code>ai-aggregs.catalog.json</code>，并在 config.toml 设 <code>model_catalog_json</code> 指向它。<b>替换</b>（非合并）内置列表——/model 将仅显示这些模型，Codex 内置模型不再显示。需 codex 已安装（取 <code>codex debug models --bundled</code> 作模板）。注：Codex 桌面版有已知显示 bug，建议用 codex CLI/TUI。
      </p>
      <div class="row">
        <label>启用模型目录</label>
        <div class="inline-toggle">
          <label class="toggle">
            <input
              type="checkbox"
              :checked="form.enable_model_catalog ?? false"
              @change="form.enable_model_catalog = ($event.target as HTMLInputElement).checked"
            />
            <span class="slider"></span>
          </label>
          <span>在 /model 列出下列模型（设 model_catalog_json）</span>
        </div>
      </div>
      <div class="row">
        <label>模型清单</label>
        <MultiCombobox
          :model-value="form.catalog_models ?? []"
          :options="consumerModels"
          placeholder="模型名，回车添加（可从网关模型选，也可手输任意）"
          @update:model-value="(v: string[]) => (form.catalog_models = v)"
        />
      </div>
    </div>

    <!-- 保存栏 -->
    <div class="actions">
      <span class="save-hint">
        整体写回受管字段 · 其它 provider 表 / 顶层键原样保留 · 保存前自动备份
      </span>
      <button class="btn btn-primary" :disabled="saving" @click="save">
        {{ saving ? '保存中…' : `保存到 ${fileBaseName}` }}
      </button>
    </div>
  </div>
</template>

<style src="./index.css" scoped></style>
