<script setup lang="ts">
/**
 * 通用 Modal 壳 —— 提供遮罩 + 居中卡片 + 关闭按钮 + ESC 键关闭。
 * 内容通过默认 slot 传入；header 通过具名 slot 可选传入。
 *
 * 用法：
 *   <AppModal :open="visible" @close="visible = false">
 *     <template #header>标题</template>
 *     ...内容...
 *   </AppModal>
 */
import { watch, onUnmounted } from 'vue'

const props = withDefaults(
  defineProps<{
    open: boolean
    /** 点击遮罩是否关闭，默认 true */
    closeOnOverlay?: boolean
  }>(),
  { closeOnOverlay: true }
)

const emit = defineEmits<{ close: [] }>()

function onOverlayClick() {
  if (props.closeOnOverlay) emit('close')
}

function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape' && props.open) emit('close')
}

// ESC 关闭：仅在 open 时挂载监听
watch(
  () => props.open,
  (v) => {
    if (v) {
      document.addEventListener('keydown', onKey)
    } else {
      document.removeEventListener('keydown', onKey)
    }
  }
)
onUnmounted(() => document.removeEventListener('keydown', onKey))
</script>

<template>
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="open" class="app-modal-overlay" @click.self="onOverlayClick">
        <div class="app-modal">
          <div v-if="$slots.header" class="app-modal-header">
            <slot name="header" />
          </div>
          <button class="app-modal-close" aria-label="关闭" @click="emit('close')">✕</button>
          <div class="app-modal-body">
            <slot />
          </div>
          <div v-if="$slots.footer" class="app-modal-footer">
            <slot name="footer" />
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.app-modal-overlay {
  position: fixed;
  inset: 0;
  background: rgba(31, 30, 30, 0.4);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 2100;
  padding: 16px;
}
.app-modal {
  position: relative;
  background: var(--bg);
  border: 1px solid var(--border-weak);
  border-radius: var(--r-md);
  width: 100%;
  max-width: 480px;
  max-height: 90vh;
  display: flex;
  flex-direction: column;
  box-shadow: 0 6px 24px rgba(0, 0, 0, 0.12);
}
.app-modal-header {
  padding: 14px 18px 8px;
  padding-right: 36px;
  font-size: 14px;
  font-weight: 600;
  color: var(--text-strong);
}
.app-modal-close {
  position: absolute;
  top: 10px;
  right: 12px;
  background: none;
  border: none;
  color: var(--text-weak);
  font-size: 14px;
  cursor: pointer;
  width: 22px;
  height: 22px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: var(--r-sm);
  transition: var(--transition);
}
.app-modal-close:hover {
  background: var(--bg-weak);
  color: var(--text-strong);
}
.app-modal-body {
  padding: 14px 18px;
  overflow-y: auto;
  flex: 1;
  font-size: 13px;
  color: var(--text);
  line-height: 1.6;
  /* 默认 flex 列布局，让多个 slot 子元素自动有间距；
     单段落（如 confirm 的 <p>）也不受影响 */
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.app-modal-footer {
  padding: 10px 18px 16px;
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

/* 动画：遮罩淡入 + 卡片从下方轻微上移 */
.modal-enter-active,
.modal-leave-active {
  transition: opacity 180ms ease;
}
.modal-enter-active .app-modal,
.modal-leave-active .app-modal {
  transition:
    transform 180ms ease,
    opacity 180ms ease;
}
.modal-enter-from,
.modal-leave-to {
  opacity: 0;
}
.modal-enter-from .app-modal,
.modal-leave-to .app-modal {
  transform: translateY(8px);
  opacity: 0;
}
</style>
