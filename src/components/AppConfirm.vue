<script setup lang="ts">
/**
 * 全局确认/提示对话框 —— 由 useDialog().modal 单例驱动。
 *   - alert 模式（alertMode=true）：仅显示确认按钮
 *   - confirm 模式（alertMode=false）：显示取消 + 确认按钮
 *
 * 由 App.vue 挂载一次，根据 useDialog().modal 自动渲染。
 */
import AppModal from './AppModal.vue'
import { useDialog } from '../composables/useDialog'

const { modal, resolveModal } = useDialog()

function onCancel() {
  resolveModal(false)
}
function onConfirm() {
  resolveModal(true)
}
</script>

<template>
  <AppModal :open="!!modal" :close-on-overlay="false" :z-index="2200" @close="onCancel">
    <template v-if="modal?.options.title" #header>{{ modal.options.title }}</template>

    <p class="confirm-message">{{ modal?.options.message }}</p>

    <template #footer>
      <button v-if="modal && !modal.alertMode" class="btn btn-secondary" @click="onCancel">
        {{ modal?.options.cancelText ?? '取消' }}
      </button>
      <button
        class="btn"
        :class="modal?.options.danger ? 'btn-danger' : 'btn-primary'"
        @click="onConfirm"
      >
        {{ modal?.options.confirmText ?? '确定' }}
      </button>
    </template>
  </AppModal>
</template>

<style scoped>
.confirm-message {
  margin: 0;
  white-space: pre-wrap;
  word-break: break-word;
}

/* 危险确认按钮：红色描边，hover 实心 */
.btn-danger {
  background: var(--bg);
  color: var(--red);
  border: 1px solid var(--red);
}
.btn-danger:hover {
  background: var(--red);
  color: var(--text-inverted);
}
</style>
