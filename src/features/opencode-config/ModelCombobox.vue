<script setup lang="ts">
import { ref, computed, nextTick } from 'vue'

const props = defineProps<{
  modelValue: string | null | undefined
  options: string[]
  placeholder?: string
  /** 候选正在异步加载中（下拉面板显示加载提示，避免候选突然增多抖动） */
  loading?: boolean
}>()
const emit = defineEmits<{ 'update:modelValue': [value: string | null] }>()

const open = ref(false)
const inputEl = ref<HTMLInputElement | null>(null)
const query = ref('')

/** 输入框显示值：始终与 modelValue 同步（可手动编辑） */
const display = computed({
  get: () => props.modelValue ?? '',
  set: (v: string) => emit('update:modelValue', v === '' ? null : v)
})

/** 下拉候选：按当前输入文本过滤 */
const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  const base = props.options
  if (!q) return base
  return base.filter((o) => o.toLowerCase().includes(q))
})

function onFocus() {
  query.value = ''
  open.value = true
}
async function onBlur() {
  // 延迟关闭，让 mousedown 选值先生效
  await nextTick()
  setTimeout(() => {
    open.value = false
  }, 150)
}
function pick(val: string) {
  emit('update:modelValue', val)
  open.value = false
  inputEl.value?.blur()
}
function clearVal() {
  emit('update:modelValue', null)
  open.value = false
  inputEl.value?.focus()
}
</script>

<template>
  <div class="combo">
    <input
      ref="inputEl"
      v-model="display"
      class="combo-input"
      :placeholder="placeholder"
      @focus="onFocus"
      @blur="onBlur"
      @input="query = ($event.target as HTMLInputElement).value"
    />
    <div v-if="open" class="combo-menu">
      <div class="combo-item combo-empty" @mousedown.prevent="clearVal">— 未设置 —</div>
      <div v-if="loading" class="combo-item combo-loading">加载候选中…</div>
      <template v-else>
        <div v-if="!filtered.length" class="combo-item combo-none">无匹配项（可手动输入）</div>
        <div
          v-for="opt in filtered"
          :key="opt"
          class="combo-item"
          :class="{ active: opt === modelValue }"
          @mousedown.prevent="pick(opt)"
        >
          {{ opt }}
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.combo {
  position: relative;
  width: 100%;
  max-width: 420px;
}
/* input 样式与系统 .row > input / .row > select 完全一致，无额外装饰 */
.combo-input {
  width: 100%;
  background: var(--bg-weak);
  border: 1px solid var(--border-weak);
  border-radius: var(--r-md);
  padding: 8px 12px;
  color: var(--text-strong);
  font-size: 12px;
  font-family: inherit;
  outline: none;
  transition: var(--transition);
  box-sizing: border-box;
}
.combo-input:focus {
  background: var(--bg-interactive-weaker);
  border-color: var(--text-strong);
  box-shadow: 0 0 0 3px var(--bg-interactive);
}
.combo-input::placeholder {
  color: var(--text-weaker);
}
/* 自定义下拉面板 */
.combo-menu {
  position: absolute;
  top: calc(100% + 3px);
  left: 0;
  right: 0;
  z-index: 50;
  background: var(--bg);
  border: 1px solid var(--text-strong);
  border-radius: var(--r-md);
  box-shadow: 0 6px 20px rgba(31, 30, 30, 0.12);
  max-height: 240px;
  overflow-y: auto;
  padding: 4px;
}
.combo-menu::-webkit-scrollbar {
  width: 6px;
}
.combo-menu::-webkit-scrollbar-thumb {
  background: var(--border);
  border-radius: 3px;
}
.combo-item {
  padding: 7px 10px;
  font-size: 12px;
  color: var(--text);
  border-radius: var(--r-sm);
  cursor: pointer;
  transition: var(--transition);
  user-select: none;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.combo-item:hover {
  background: var(--bg-weak);
  color: var(--text-strong);
}
.combo-item.active {
  background: var(--bg-weak);
  color: var(--text-strong);
  font-weight: 600;
}
.combo-empty {
  color: var(--text-weak);
  font-style: italic;
}
.combo-empty:hover {
  color: var(--text);
}
.combo-none {
  color: var(--text-weaker);
  cursor: default;
  font-style: italic;
}
.combo-none:hover {
  background: transparent;
  color: var(--text-weaker);
}
.combo-loading {
  color: var(--text-weak);
  cursor: progress;
  font-style: italic;
}
.combo-loading:hover {
  background: transparent;
  color: var(--text-weak);
}
</style>
