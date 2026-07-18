<script setup lang="ts">
import { useProviderList } from './index'
import { normalizeKey } from '../../api/commands'
import AppModal from '../../components/AppModal.vue'

defineProps<{ gatewayRunning: boolean }>()
const {
  config,
  loading,
  sortedProviders,
  dragIdx,
  dragOverIdx,
  onHandleMouseDown,
  modalMode,
  editingProvider,
  modelInput,
  keyInput,
  onToggleProvider,
  onToggleKey,
  openAdd,
  openEdit,
  closeModal,
  submitModal,
  deleteFromModal,
  modalAddModel,
  modalRemoveModel,
  modalAddKey,
  modalRemoveKey,
  getRuntime,
  keyRuntime,
  maskKey,
  iconFor,
  keyReleaseTime,
  fmtTime
} = useProviderList()
</script>

<template>
  <div v-if="config" :class="{ 'provider-dragging': dragIdx !== -1 }">
    <!-- 标题栏 -->
    <div class="header-row">
      <div>
        <h1 class="page-title">供应商管理</h1>
        <p class="page-sub">管理上游 API 供应商及密钥</p>
      </div>
      <div class="header-actions">
        <button class="btn btn-primary sm" @click="openAdd">+ 添加提供商</button>
      </div>
    </div>

    <!-- 卡片网格 -->
    <div class="card-grid">
      <div
        v-for="{ p, idx } in sortedProviders"
        :key="p.id ?? idx"
        class="provider-card"
        :class="{ off: !p.enabled, dragging: dragIdx === idx, 'drag-over': dragOverIdx === idx }"
        :data-idx="idx"
      >
        <!-- 卡片头部（双击编辑） -->
        <div class="provider-card-header" @dblclick="openEdit(idx)">
          <div class="provider-card-left">
            <!-- 拖动排序手柄（仅此处可发起拖动） -->
            <div
              class="drag-handle"
              title="拖动以排序"
              @mousedown.stop.prevent="onHandleMouseDown($event, idx)"
            >
              <svg
                width="10"
                height="14"
                viewBox="0 0 10 14"
                fill="currentColor"
                aria-hidden="true"
              >
                <circle cx="2" cy="2" r="1.4" />
                <circle cx="8" cy="2" r="1.4" />
                <circle cx="2" cy="7" r="1.4" />
                <circle cx="8" cy="7" r="1.4" />
                <circle cx="2" cy="12" r="1.4" />
                <circle cx="8" cy="12" r="1.4" />
              </svg>
            </div>
            <div class="provider-icon">{{ iconFor(p.protocol) }}</div>
            <div class="provider-name">
              <span class="name-text">{{ p.name }}</span>
              <span class="protocol-tag">{{ p.protocol }}</span>
            </div>
          </div>
          <div class="provider-card-right">
            <label class="toggle" @click.stop>
              <input
                type="checkbox"
                :checked="p.enabled"
                :disabled="loading"
                @change="onToggleProvider(idx, ($event.target as HTMLInputElement).checked)"
              />
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
                  <input
                    type="checkbox"
                    :checked="normalizeKey(entry).enabled"
                    :disabled="loading"
                    @change="onToggleKey(p.name, ki, ($event.target as HTMLInputElement).checked)"
                  />
                  <span class="slider"></span>
                </label>
                <span class="key-value">{{ maskKey(normalizeKey(entry).key) }}</span>
                <span v-if="keyRuntime(p.name, ki)?.blacklisted" class="key-status blacklisted">
                  {{ fmtTime(keyReleaseTime(keyRuntime(p.name, ki)?.blacklist_remaining_secs)!) }}
                </span>
                <span v-else-if="!normalizeKey(entry).enabled" class="key-status disabled"
                  >已禁用</span
                >
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

    <!-- 添加 / 编辑 弹窗（统一使用 AppModal 组件） -->
    <AppModal :open="!!modalMode" :close-on-overlay="true" @close="closeModal">
      <template #header>
        {{ modalMode === 'add' ? '添加提供商' : '编辑提供商' }}
      </template>

      <div class="mf">
        <label>名称</label>
        <input v-model="editingProvider.name" class="f-input" placeholder="如 glm" />
      </div>
      <div class="mf-row">
        <!-- 协议选择 — 分段控件 -->
        <div class="mf">
          <label>协议</label>
          <div class="seg">
            <input id="proto-chat" v-model="editingProvider.protocol" type="radio" value="chat" />
            <label for="proto-chat">chat</label>
            <input
              id="proto-resp"
              v-model="editingProvider.protocol"
              type="radio"
              value="responses"
            />
            <label for="proto-resp">responses</label>
            <input
              id="proto-anth"
              v-model="editingProvider.protocol"
              type="radio"
              value="anthropic"
            />
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
        <input
          v-model="editingProvider.base_url"
          class="f-input"
          placeholder="https://api.example.com/v1"
        />
      </div>

      <!-- API Keys（先填 key 再填模型） -->
      <div class="mf">
        <label>API Keys</label>
        <div class="chip-input">
          <span v-for="(k, ki) in editingProvider.api_keys" :key="ki" class="chip">
            <span class="chip-text">{{ maskKey(normalizeKey(k).key) }}</span>
            <button class="chip-x" @click="modalRemoveKey(ki)">×</button>
          </span>
          <input
            v-model="keyInput"
            class="chip-field"
            :placeholder="editingProvider.api_keys.length ? '' : 'API Key，回车添加'"
            @keydown.enter.prevent="modalAddKey"
          />
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
          <input
            v-model="modelInput"
            class="chip-field"
            :placeholder="editingProvider.models.length ? '' : '模型名，回车添加'"
            @keydown.enter.prevent="modalAddModel"
          />
        </div>
      </div>

      <div class="mf-row">
        <div class="mf">
          <label>超时（秒）</label>
          <input v-model.number="editingProvider.timeout_secs" type="number" class="f-input" />
        </div>
      </div>

      <template #footer>
        <button
          v-if="modalMode === 'edit'"
          class="btn btn-secondary danger-btn"
          @click="deleteFromModal"
        >
          删除
        </button>
        <div class="footer-right">
          <button class="btn btn-secondary" @click="closeModal">取消</button>
          <button class="btn btn-primary" @click="submitModal">
            {{ modalMode === 'add' ? '创建' : '保存' }}
          </button>
        </div>
      </template>
    </AppModal>
  </div>
</template>

<style src="./index.css" scoped></style>
