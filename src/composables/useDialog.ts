/**
 * 全局弹窗统一管理 —— 通过 provide/inject 在 App.vue 注入一份全局状态，
 * 任意子组件通过 `useDialog()` 即可调用 toast / alert / confirm。
 *
 * 设计原则：
 *   - 单一状态源：toast 队列 + 当前 modal 都在一个 reactive 对象中
 *   - 极简 API：toast() 自动消失；alert() 需用户确认（单按钮）；
 *     confirm() 返回 Promise<boolean>（双按钮）
 *   - 视觉风格由 AppToast / AppModal / AppConfirm 组件负责
 */
import { ref, provide, inject, type Ref } from 'vue'

// ===================== 类型 =====================

export type ToastType = 'info' | 'success' | 'error'

export interface ToastItem {
  id: number
  type: ToastType
  message: string
}

export interface ModalOptions {
  /** 标题（可选，默认无） */
  title?: string
  /** 正文（必填） */
  message: string
  /** 确认按钮文案，默认「确定」 */
  confirmText?: string
  /** 取消按钮文案，默认「取消」（alert 模式无此按钮） */
  cancelText?: string
  /** 危险动作（删除等）：确认按钮显示红色 */
  danger?: boolean
}

interface ModalItem {
  id: number
  options: ModalOptions
  /** 是否为单按钮 alert 模式（true = 仅确认按钮；false = 确认+取消） */
  alertMode: boolean
  resolve: (ok: boolean) => void
}

export interface DialogState {
  toasts: Ref<ToastItem[]>
  modal: Ref<ModalItem | null>
  /** 弹出 toast，默认 2400ms 自动消失 */
  toast: (message: string, type?: ToastType, durationMs?: number) => void
  /** alert 框（单按钮），返回 Promise 在用户点击确认后 resolve */
  alert: (message: string | ModalOptions) => Promise<void>
  /** confirm 框（双按钮），返回 Promise<boolean> 表示是否确认 */
  confirm: (message: string | ModalOptions) => Promise<boolean>
  /** 内部：移除某条 toast（供 AppToast 组件调用） */
  removeToast: (id: number) => void
  /** 内部：关闭当前 modal（供 AppConfirm 调用） */
  resolveModal: (ok: boolean) => void
}

// ===================== 注入 key =====================

const DIALOG_KEY: symbol = Symbol('app-dialog')

// ===================== Provider（在 App.vue 调用一次） =====================

export function provideDialog(): DialogState {
  const toasts = ref<ToastItem[]>([])
  const modal = ref<ModalItem | null>(null)
  let nextId = 1

  function toast(message: string, type: ToastType = 'info', durationMs = 2400) {
    const id = nextId++
    toasts.value.push({ id, type, message })
    if (durationMs > 0) {
      setTimeout(() => removeToast(id), durationMs)
    }
  }

  function removeToast(id: number) {
    const idx = toasts.value.findIndex((t) => t.id === id)
    if (idx >= 0) toasts.value.splice(idx, 1)
  }

  function normalize(message: string | ModalOptions): ModalOptions {
    return typeof message === 'string' ? { message } : message
  }

  function alert(message: string | ModalOptions): Promise<void> {
    return new Promise((resolve) => {
      modal.value = {
        id: nextId++,
        options: normalize(message),
        alertMode: true,
        resolve: () => resolve()
      }
    })
  }

  function confirm(message: string | ModalOptions): Promise<boolean> {
    return new Promise((resolve) => {
      modal.value = {
        id: nextId++,
        options: normalize(message),
        alertMode: false,
        resolve: (ok) => resolve(ok)
      }
    })
  }

  function resolveModal(ok: boolean) {
    if (modal.value) {
      const r = modal.value.resolve
      modal.value = null
      r(ok)
    }
  }

  const state: DialogState = {
    toasts,
    modal,
    toast,
    alert,
    confirm,
    removeToast,
    resolveModal
  }
  provide(DIALOG_KEY, state)
  return state
}

// ===================== Consumer（任意子组件调用） =====================

export function useDialog(): DialogState {
  const state = inject<DialogState>(DIALOG_KEY)
  if (!state) {
    throw new Error('useDialog() 必须在 provideDialog() 注入的组件树内调用')
  }
  return state
}
