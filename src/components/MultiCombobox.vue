<script setup lang="ts">
import { ref, computed, nextTick } from 'vue'

const props = defineProps<{
  modelValue: string[]
  options: string[]
  placeholder?: string
}>()
const emit = defineEmits<{ 'update:modelValue': [value: string[]] }>()

const open = ref(false)
const inputEl = ref<HTMLInputElement | null>(null)
const query = ref('')

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase()
  return props.options.filter(
    (o) => !props.modelValue.includes(o) && (!q || o.toLowerCase().includes(q))
  )
})

const canAdd = computed(() => {
  const q = query.value.trim()
  if (!q) return false
  return !props.modelValue.includes(q) && !props.options.includes(q)
})

function pick(val: string) {
  if (!props.modelValue.includes(val)) {
    emit('update:modelValue', [...props.modelValue, val])
  }
}

function remove(val: string) {
  emit(
    'update:modelValue',
    props.modelValue.filter((x) => x !== val)
  )
}

function addQuery() {
  const q = query.value.trim()
  if (!q) return
  if (!props.modelValue.includes(q)) {
    emit('update:modelValue', [...props.modelValue, q])
  }
  query.value = ''
}

function onFocus() {
  open.value = true
}
async function onBlur() {
  await nextTick()
  setTimeout(() => {
    open.value = false
  }, 150)
}
function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter') {
    e.preventDefault()
    addQuery()
  } else if (e.key === 'Backspace' && !query.value && props.modelValue.length) {
    remove(props.modelValue[props.modelValue.length - 1])
  }
}
</script>

<template>
  <div class="mcombo">
    <div class="mcombo-field" @mousedown.prevent="inputEl?.focus()">
      <span v-for="id in modelValue" :key="id" class="mcombo-tag">
        <span class="mcombo-tag-nm">{{ id }}</span>
        <button
          type="button"
          class="mcombo-tag-x"
          title="移除"
          @mousedown.prevent.stop="remove(id)"
        >×</button>
      </span>
      <input
        ref="inputEl"
        v-model="query"
        class="mcombo-input"
        :placeholder="modelValue.length ? '' : placeholder"
        @focus="onFocus"
        @blur="onBlur"
        @keydown="onKeydown"
      />
    </div>
    <div v-if="open" class="mcombo-menu">
      <div v-if="!filtered.length && !canAdd" class="mcombo-item mcombo-none">无更多可选项</div>
      <div
        v-for="opt in filtered"
        :key="opt"
        class="mcombo-item"
        @mousedown.prevent="pick(opt)"
      >
        {{ opt }}
      </div>
      <div v-if="canAdd" class="mcombo-item mcombo-add" @mousedown.prevent="addQuery">
        添加「{{ query.trim() }}」
      </div>
    </div>
  </div>
</template>

<style scoped>
.mcombo {
  position: relative;
  width: 100%;
  max-width: 560px;
}
.mcombo-field {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px;
  background: var(--bg-weak);
  border: 1px solid var(--border-weak);
  border-radius: var(--r-md);
  padding: 5px 7px;
  cursor: text;
  transition: var(--transition);
  min-height: 34px;
  box-sizing: border-box;
}
.mcombo-field:focus-within {
  background: var(--bg-interactive-weaker);
  border-color: var(--text-strong);
  box-shadow: 0 0 0 3px var(--bg-interactive);
}
.mcombo-input {
  flex: 1;
  min-width: 130px;
  border: none;
  outline: none;
  background: transparent;
  color: var(--text-strong);
  font-size: 12px;
  font-family: inherit;
  padding: 2px 0;
}
.mcombo-input::placeholder {
  color: var(--text-weaker);
}
.mcombo-tag {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  background: var(--bg);
  border: 1px solid var(--border-weak);
  border-radius: var(--r-sm);
  padding: 1px 2px 1px 7px;
  font-size: 11px;
  white-space: nowrap;
  user-select: none;
}
.mcombo-tag:hover {
  border-color: var(--border);
}
.mcombo-tag-nm {
  color: var(--text);
  font-size: 10px;
}
.mcombo-tag-x {
  background: none;
  border: none;
  color: var(--text-weak);
  cursor: pointer;
  font-size: 12px;
  line-height: 1;
  padding: 0 2px;
  transition: var(--transition);
}
.mcombo-tag-x:hover {
  color: var(--red);
}
.mcombo-menu {
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
.mcombo-menu::-webkit-scrollbar {
  width: 6px;
}
.mcombo-menu::-webkit-scrollbar-thumb {
  background: var(--border);
  border-radius: 3px;
}
.mcombo-item {
  display: flex;
  align-items: center;
  gap: 6px;
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
.mcombo-item:hover {
  background: var(--bg-weak);
  color: var(--text-strong);
}
.mcombo-add {
  color: var(--text-weak);
}
.mcombo-none {
  color: var(--text-weaker);
  cursor: default;
  font-style: italic;
}
.mcombo-none:hover {
  background: transparent;
  color: var(--text-weaker);
}
</style>
