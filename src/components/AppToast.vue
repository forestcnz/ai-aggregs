<script setup lang="ts">
/**
 * 全局 Toast 容器 —— 右上角浮层，多条堆叠。
 * 由 App.vue 挂载一次，从 useDialog().toasts 渲染所有 toast。
 */
import { useDialog } from '../composables/useDialog'

const { toasts, removeToast } = useDialog()
</script>

<template>
  <Teleport to="body">
    <TransitionGroup tag="div" name="toast" class="app-toast-container">
      <div
        v-for="t in toasts"
        :key="t.id"
        class="app-toast"
        :class="`toast-${t.type}`"
        @click="removeToast(t.id)"
      >
        <span class="toast-icon">{{
          t.type === 'success' ? '✓' : t.type === 'error' ? '✕' : '•'
        }}</span>
        <span class="toast-msg">{{ t.message }}</span>
      </div>
    </TransitionGroup>
  </Teleport>
</template>

<style scoped>
.app-toast-container {
  position: fixed;
  top: 48px;
  right: 24px;
  z-index: 2000;
  display: flex;
  flex-direction: column;
  gap: 6px;
  pointer-events: none;
}
.app-toast {
  pointer-events: auto;
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 7px 13px;
  border: 1px solid var(--border-weak);
  border-radius: var(--r-md);
  background: var(--bg);
  color: var(--text);
  font-size: 12px;
  font-weight: 500;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.04);
  cursor: pointer;
  max-width: 360px;
}
.toast-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  font-size: 10px;
  font-weight: 700;
  line-height: 1;
  flex-shrink: 0;
}
/* info：中性灰 */
.toast-info .toast-icon {
  background: var(--text-weaker);
  color: var(--bg);
}
/* success：绿色（沿用 --green） */
.toast-success {
  color: var(--green);
  border-color: var(--green);
}
.toast-success .toast-icon {
  background: var(--green);
  color: var(--text-inverted);
}
/* error：红色（沿用 --red） */
.toast-error {
  color: var(--red);
  border-color: var(--red);
}
.toast-error .toast-icon {
  background: var(--red);
  color: var(--text-inverted);
}
.toast-msg {
  white-space: pre-wrap;
  word-break: break-word;
}

/* TransitionGroup 动画：从右侧滑入/淡出。
 * 不用 position:absolute（会导致高度塌陷变扁），toast 原地淡出即可。 */
.toast-enter-active,
.toast-leave-active {
  transition:
    opacity 180ms ease,
    transform 180ms ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateX(12px);
}
</style>
