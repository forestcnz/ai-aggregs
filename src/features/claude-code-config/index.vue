<script setup lang="ts">
import {
  useClaudeCodeConfig,
  CC_BASE_URL_KEY,
  CC_AUTH_TOKEN_KEY,
  CC_MODEL_FIELDS,
  isSecretKey
} from './index'
import ModelCombobox from '../opencode-config/ModelCombobox.vue'

defineProps<{ version?: string | null }>()

const {
  form,
  loading,
  saving,
  filePath,
  fileExists,
  editingKeys,
  modelSelectOptions,
  customEntries,
  fileBaseName,
  envCount,
  customCount,
  load,
  save,
  linkToGateway,
  getEnv,
  setEnv,
  startEdit,
  endEdit,
  maskedValue,
  addCustomEntry,
  removeEntry,
  onCustomKeyInput
} = useClaudeCodeConfig()
</script>

<template>
  <div v-if="loading" class="oc-loading">加载中…</div>
  <div v-else-if="form" class="oc-page">
    <h1 class="page-title">
      Claude Code 配置
      <span v-if="version" class="file-tag" title="claude code 版本">v{{ version }}</span>
      <span class="file-tag">{{ fileBaseName }}</span>
    </h1>
    <p class="page-sub">
      编辑 Claude Code 的 <code>env</code> 环境变量 · 把它指向本网关 · 其余字段原样保留
    </p>

    <!-- 工具栏 -->
    <div class="oc-toolbar">
      <div class="oc-path">
        <span class="pth">{{ filePath || '~/.claude/settings.json' }}</span>
        <span v-if="fileExists" class="meta">· 已存在</span>
        <span v-else class="meta">· 不存在（保存时新建）</span>
      </div>
      <div class="oc-actions">
        <button class="btn btn-secondary sm" @click="linkToGateway">一键对接网关</button>
        <button class="btn btn-secondary sm" @click="load">重新加载</button>
      </div>
    </div>

    <!-- 01 连接 -->
    <div class="group">
      <h3>
        <span class="gn">01</span>连接
        <span class="cnt">· 指向本网关（Anthropic 协议）</span>
      </h3>
      <p class="oc-blocked-tip">
        <code>ANTHROPIC_BASE_URL</code> 把请求转发到网关；但<b>仅设 base URL 不会替换登录订阅</b>——
        还需 <code>ANTHROPIC_AUTH_TOKEN</code> 作为网关凭证（即本网关的 consumer key）。
        点击工具栏「一键对接网关」可同时填入两者。注意：不要同时设置
        <code>ANTHROPIC_API_KEY</code>（二者并存会被上游拒绝）。
      </p>
      <div class="row">
        <label>base URL</label>
        <input
          class="inp mono"
          :value="getEnv(CC_BASE_URL_KEY)"
          placeholder="http://127.0.0.1:8000（不带 /v1）"
          @input="setEnv(CC_BASE_URL_KEY, ($event.target as HTMLInputElement).value)"
        />
      </div>
      <div class="row">
        <label>auth token</label>
        <div class="key-field">
          <template
            v-if="
              isSecretKey(CC_AUTH_TOKEN_KEY) &&
              getEnv(CC_AUTH_TOKEN_KEY) &&
              !editingKeys.has(CC_AUTH_TOKEN_KEY)
            "
          >
            <span class="key-display" @click="startEdit(CC_AUTH_TOKEN_KEY)">
              {{ maskedValue(CC_AUTH_TOKEN_KEY) }}
            </span>
          </template>
          <template v-else>
            <input
              class="inp mono"
              :value="getEnv(CC_AUTH_TOKEN_KEY)"
              placeholder="ANTHROPIC_AUTH_TOKEN（网关 consumer key）"
              autofocus
              @input="setEnv(CC_AUTH_TOKEN_KEY, ($event.target as HTMLInputElement).value)"
              @blur="endEdit(CC_AUTH_TOKEN_KEY)"
            />
          </template>
        </div>
      </div>
    </div>

    <!-- 02 模型 -->
    <div class="group">
      <h3>
        <span class="gn">02</span>模型
        <span class="cnt">· 候选来自网关 consumer models</span>
      </h3>
      <div class="cc-models">
        <div v-for="f in CC_MODEL_FIELDS" :key="f.key" class="cc-field">
          <label>{{ f.label }} <span class="cc-field-hint">{{ f.hint }}</span></label>
          <ModelCombobox
            :model-value="getEnv(f.key)"
            :options="modelSelectOptions"
            :placeholder="`留空回退默认（可下拉或手动输入）`"
            @update:model-value="(v) => setEnv(f.key, v ?? '')"
          />
        </div>
      </div>
    </div>

    <!-- 03 其他环境变量 -->
    <div class="group">
      <h3>
        <span class="gn">03</span>其他环境变量
        <span class="cnt">· {{ customCount }} 条 · 共 {{ envCount }} 条</span>
      </h3>
      <p class="oc-blocked-tip">
        未归入上方精选字段的 <code>env</code> 条目在此自由编辑（如
        <code>API_TIMEOUT_MS</code>、<code>CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC</code>）。
        含 TOKEN/SECRET/PASSWORD 的值默认掩码，点按切换明文。
      </p>

      <div v-if="!customEntries.length" class="oc-empty">暂无其他变量，点击下方添加</div>

      <div class="cc-env-list">
        <div v-for="(e, i) in customEntries" :key="i" class="cc-env-row">
          <input
            class="inp mono cc-env-key"
            :value="e.key"
            placeholder="变量名（如 API_TIMEOUT_MS）"
            @input="onCustomKeyInput(e, ($event.target as HTMLInputElement).value)"
          />
          <template v-if="e.secret && e.value && !editingKeys.has(e.key)">
            <span class="key-display cc-env-val" @click="startEdit(e.key)">
              {{ maskedValue(e.key) }}
            </span>
          </template>
          <template v-else>
            <input
              class="inp mono cc-env-val"
              :value="e.value"
              placeholder="值"
              @input="e.value = ($event.target as HTMLInputElement).value"
              @blur="endEdit(e.key)"
            />
          </template>
          <button class="btn btn-secondary sm cc-env-del" @click="removeEntry(e)">删除</button>
        </div>
      </div>

      <div class="oc-add">
        <button class="btn btn-secondary sm" @click="addCustomEntry">+ 添加变量</button>
        <span class="hint">空值条目在保存时会被丢弃</span>
      </div>
    </div>

    <!-- 保存栏 -->
    <div class="actions">
      <span class="save-hint">
        整体写回 <code>env</code> 段 · enabledPlugins / statusLine 等未管理字段原样保留 · 保存前自动备份
      </span>
      <button class="btn btn-primary" :disabled="saving" @click="save">
        {{ saving ? '保存中…' : `保存到 ${fileBaseName}` }}
      </button>
    </div>
  </div>
</template>

<style src="./index.css" scoped></style>
