<script setup lang="ts">
import { ref, nextTick, onMounted, computed } from 'vue'
import { getConfig, type GatewayStatus, type Config } from '../api/commands'

const props = defineProps<{ status: GatewayStatus }>()

// ---- 协议类型 ----
type ChatProtocol = 'chat' | 'responses' | 'anthropic'
const PROTOCOL_ENDPOINT: Record<ChatProtocol, string> = {
  chat: '/v1/chat/completions',
  responses: '/v1/responses',
  anthropic: '/v1/messages'
}

// ---- 状态 ----
const config = ref<Config | null>(null)
const protocol = ref<ChatProtocol>('chat')
const selectedModel = ref('')
const input = ref('')
const sending = ref(false)
const dialogMsg = ref('')
const messages = ref<ChatMsg[]>([])
const scrollEl = ref<HTMLElement | null>(null)
const textareaRef = ref<HTMLTextAreaElement | null>(null)
// 当前请求的 AbortController（用于中止流式请求）
let abortCtrl: AbortController | null = null

interface ChatMsg {
  role: 'user' | 'assistant'
  content: string
  reasoning: string
  thinking: boolean
  error?: string
}

// ---- 计算属性 ----
const models = computed(() => config.value?.consumer.models ?? [])
const gatewayUrl = computed(() => {
  if (!props.status.running || !props.status.listen_addr) return ''
  return `http://${props.status.listen_addr}`
})
const apiKey = computed(() => config.value?.consumer.api_keys?.[0] ?? '')

// ---- 生命周期 ----
onMounted(async () => {
  try {
    config.value = await getConfig()
    if (models.value.length > 0) {
      selectedModel.value = models.value[0]
    }
  } catch (e) {
    console.error('加载配置失败', e)
  }
})

// ---- 工具函数 ----
function showDialog(msg: string) {
  dialogMsg.value = msg
}
function closeDialog() {
  dialogMsg.value = ''
  nextTick(() => textareaRef.value?.focus())
}
async function scrollToBottom() {
  await nextTick()
  if (scrollEl.value) {
    scrollEl.value.scrollTop = scrollEl.value.scrollHeight
  }
}

// ---- 按协议构建请求体 ----
function buildRequestBody(
  model: string,
  sendMessages: { role: string; content: string }[]
): Record<string, unknown> {
  if (protocol.value === 'responses') {
    // Responses 协议：input 字段，每条消息含 type/content 结构；启用 reasoning summary
    const input = sendMessages.map((m) => {
      if (m.role === 'user') {
        return { type: 'message', role: 'user', content: [{ type: 'input_text', text: m.content }] }
      } else if (m.role === 'assistant') {
        return {
          type: 'message',
          role: 'assistant',
          content: [{ type: 'output_text', text: m.content }]
        }
      } else {
        return { type: 'message', role: m.role, content: [{ type: 'input_text', text: m.content }] }
      }
    })
    return {
      model,
      input,
      stream: true,
      // 启用推理摘要，使模型返回思考过程
      reasoning: { effort: 'medium', summary: 'auto' }
    }
  } else if (protocol.value === 'anthropic') {
    // Anthropic 协议：messages 字段，需要 max_tokens；启用 thinking
    return {
      model,
      messages: sendMessages,
      max_tokens: 16000,
      stream: true,
      // 启用 extended thinking
      thinking: { type: 'enabled', budget_tokens: 10000 }
    }
  } else {
    // Chat 协议：messages 字段；启用 thinking
    return {
      model,
      messages: sendMessages,
      stream: true,
      reasoning_effort: 'medium',
      thinking: { type: 'enabled' }
    }
  }
}

// ---- 按协议构建请求头 ----
function buildHeaders(): Record<string, string> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  if (apiKey.value) {
    // Anthropic 用 x-api-key，其余用 Bearer；网关两种都支持
    if (protocol.value === 'anthropic') {
      headers['x-api-key'] = apiKey.value
    } else {
      headers['Authorization'] = `Bearer ${apiKey.value}`
    }
  }
  return headers
}

// ---- 按协议解析 SSE data ----
function handleSseData(data: string, idx: number): boolean {
  const msg = messages.value[idx]
  if (!msg) return false

  if (protocol.value === 'chat') {
    return handleChatData(data, msg)
  } else if (protocol.value === 'responses') {
    return handleResponsesData(data, msg)
  } else {
    return handleAnthropicData(data, msg)
  }
}

// Chat 协议 SSE 解析：choices[0].delta.content / reasoning_content
function handleChatData(data: string, msg: ChatMsg): boolean {
  if (data === '[DONE]') {
    msg.thinking = false
    return true
  }
  try {
    const json = JSON.parse(data)
    const delta = json.choices?.[0]?.delta
    if (!delta) return false
    if (delta.reasoning_content) {
      msg.reasoning += delta.reasoning_content
      msg.thinking = true
    }
    if (delta.content) {
      msg.content += delta.content
      msg.thinking = false
    }
  } catch {
    // 忽略无法解析的行
  }
  return false
}

// Responses 协议 SSE 解析：response.output_text.delta / response.reasoning_summary_text.delta / response.completed
function handleResponsesData(data: string, msg: ChatMsg): boolean {
  try {
    const json = JSON.parse(data)
    const t = json.type ?? ''
    if (t === 'response.reasoning_summary_text.delta') {
      // 推理摘要增量 -> 思考过程
      const d = json.delta
      if (typeof d === 'string') {
        msg.reasoning += d
        msg.thinking = true
      }
    } else if (t === 'response.reasoning_summary_text.done') {
      // 推理摘要完成（text 字段含完整文本，但增量已累积，无需再追加）
      msg.thinking = true
    } else if (t === 'response.output_text.delta') {
      const d = json.delta
      if (typeof d === 'string') {
        msg.content += d
        msg.thinking = false
      }
    } else if (t === 'response.completed') {
      msg.thinking = false
      return true
    } else if (t === 'response.failed' || t === 'response.error') {
      msg.thinking = false
      return true
    }
  } catch {
    // 忽略无法解析的行
  }
  return false
}

// Anthropic 协议 SSE 解析：content_block_delta(thinking_delta/text_delta) / message_stop
function handleAnthropicData(data: string, msg: ChatMsg): boolean {
  try {
    const json = JSON.parse(data)
    const t = json.type ?? ''
    if (t === 'content_block_start') {
      const cb = json.content_block
      if (cb?.type === 'thinking') {
        msg.thinking = true
      }
    } else if (t === 'content_block_delta') {
      const delta = json.delta
      if (!delta) return false
      if (delta.type === 'thinking_delta') {
        msg.reasoning += delta.thinking ?? ''
        msg.thinking = true
      } else if (delta.type === 'text_delta') {
        msg.content += delta.text ?? ''
        msg.thinking = false
      }
    } else if (t === 'message_stop') {
      msg.thinking = false
      return true
    }
  } catch {
    // 忽略无法解析的行
  }
  return false
}

// ---- 发送消息 ----
async function send() {
  const text = input.value.trim()
  if (!text || sending.value) return

  // 前置检查
  if (!gatewayUrl.value) {
    showDialog('网关未运行，请先在「网关状态」页启动网关')
    return
  }
  if (!selectedModel.value) {
    showDialog('请先选择模型')
    return
  }

  // 加入用户消息
  messages.value.push({ role: 'user', content: text, reasoning: '', thinking: false })
  input.value = ''
  await scrollToBottom()

  // 构建发送给网关的消息列表（在加入助手占位之前）
  const sendMessages = messages.value.map((m) => ({ role: m.role, content: m.content }))

  // 加入助手消息占位（push 后通过索引访问 reactive 代理对象）
  messages.value.push({ role: 'assistant', content: '', reasoning: '', thinking: true })
  const assistantIdx = messages.value.length - 1
  await scrollToBottom()

  sending.value = true
  const controller = new AbortController()
  abortCtrl = controller

  try {
    const resp = await fetch(`${gatewayUrl.value}${PROTOCOL_ENDPOINT[protocol.value]}`, {
      method: 'POST',
      headers: buildHeaders(),
      body: JSON.stringify(buildRequestBody(selectedModel.value, sendMessages)),
      signal: controller.signal
    })

    if (!resp.ok) {
      const errText = await resp.text().catch(() => '')
      const msg = messages.value[assistantIdx]
      msg.error = `请求失败 (${resp.status}): ${errText}`
      msg.thinking = false
      return
    }

    const reader = resp.body?.getReader()
    if (!reader) {
      const msg = messages.value[assistantIdx]
      msg.error = '无法读取响应流'
      msg.thinking = false
      return
    }

    const decoder = new TextDecoder()
    let buffer = ''
    let done = false

    while (!done) {
      const { done: readerDone, value } = await reader.read()
      if (readerDone) break
      buffer += decoder.decode(value, { stream: true })

      // 按 SSE 事件边界（空行）分割
      const parts = buffer.split('\n\n')
      buffer = parts.pop() ?? ''
      for (const part of parts) {
        // SSE 事件可能含多行（event: + data:），提取 data: 行
        const dataLine = part.split('\n').find((l) => l.startsWith('data:'))
        if (!dataLine) continue
        const data = dataLine.slice(5).trim()
        if (handleSseData(data, assistantIdx)) {
          done = true
          break
        }
        await scrollToBottom()
      }
    }
    // 处理残留 buffer
    if (buffer.trim().startsWith('data:')) {
      const data = buffer.trim().slice(5).trim()
      handleSseData(data, assistantIdx)
    }
    const msg = messages.value[assistantIdx]
    msg.thinking = false
    await scrollToBottom()
  } catch (e: any) {
    const msg = messages.value[assistantIdx]
    if (msg) {
      if (e.name === 'AbortError') {
        msg.content += '\n\n[已中断]'
      } else {
        msg.error = e.message || String(e)
      }
      msg.thinking = false
    }
  } finally {
    sending.value = false
    abortCtrl = null
  }
}

// 中止当前流式请求
function stop() {
  abortCtrl?.abort()
}

// 清空对话（不持久化，仅清空内存）
function clearChat() {
  messages.value = []
}

// Enter 发送，Shift+Enter 换行
function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    send()
  }
}
</script>

<template>
  <div class="chat-page">
    <div class="chat-header">
      <div class="chat-controls">
        <select v-model="protocol" class="proto-select" :disabled="sending">
          <option value="chat">Chat</option>
          <option value="responses">Responses</option>
          <option value="anthropic">Anthropic</option>
        </select>
        <select v-model="selectedModel" class="model-select" :disabled="sending">
          <option value="" disabled>选择模型...</option>
          <option v-for="m in models" :key="m" :value="m">{{ m }}</option>
        </select>
        <button
          class="btn btn-secondary sm"
          :disabled="sending || messages.length === 0"
          @click="clearChat"
        >
          清空
        </button>
      </div>
    </div>

    <!-- 网关未运行提示 -->
    <div v-if="!status.running" class="notice">网关未运行，请先在「网关状态」页启动网关。</div>
    <!-- 无可用模型提示 -->
    <div v-else-if="models.length === 0" class="notice">
      没有可用的模型，请先在「提供商」页配置并启用提供商。
    </div>

    <!-- 消息列表 -->
    <div ref="scrollEl" class="messages">
      <div v-if="messages.length === 0" class="empty-hint">输入消息开始对话</div>

      <div v-for="(msg, i) in messages" :key="i" class="msg-row" :class="msg.role">
        <div class="msg-avatar">{{ msg.role === 'user' ? '我' : 'AI' }}</div>
        <div class="msg-bubble">
          <!-- 错误 -->
          <div v-if="msg.error" class="msg-error">{{ msg.error }}</div>

          <!-- THINKING（仅助手且存在 reasoning 时显示，始终展开） -->
          <div v-if="msg.role === 'assistant' && msg.reasoning" class="thinking-block">
            <div class="thinking-header">
              <span class="thinking-label">THINKING</span>
              <span v-if="msg.thinking" class="thinking-dots"
                ><span></span><span></span><span></span
              ></span>
            </div>
            <div class="thinking-content">
              {{ msg.reasoning }}
            </div>
          </div>

          <!-- 回复内容 -->
          <div v-if="msg.content" class="msg-content">{{ msg.content }}</div>

          <!-- 等待回复占位 -->
          <div
            v-if="
              msg.role === 'assistant' &&
              !msg.content &&
              !msg.reasoning &&
              !msg.error &&
              msg.thinking
            "
            class="msg-waiting"
          >
            <span class="dots"><span></span><span></span><span></span></span>
          </div>
        </div>
      </div>
    </div>

    <!-- 弹窗 -->
    <div v-if="dialogMsg" class="dialog-overlay" @click.self="closeDialog">
      <div class="dialog-box">
        <p class="dialog-text">{{ dialogMsg }}</p>
        <button class="btn btn-primary" @click="closeDialog">确定</button>
      </div>
    </div>

    <!-- 输入区 -->
    <div class="input-area">
      <textarea
        ref="textareaRef"
        v-model="input"
        class="input-box"
        placeholder="输入消息，Enter 发送，Shift+Enter 换行"
        :disabled="sending"
        rows="3"
        @keydown="onKeydown"
      />
      <button
        v-if="!sending"
        class="btn btn-primary send-btn"
        @click="send"
      >
        发送
      </button>
      <button v-else class="btn btn-stop send-btn" @click="stop">停止</button>
    </div>
  </div>
</template>

<style scoped>
.chat-page {
  display: flex;
  flex-direction: column;
  height: calc(100vh - 36px - 48px);
}
.chat-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 14px;
  flex-shrink: 0;
}
.chat-controls {
  display: flex;
  gap: 7px;
  align-items: center;
}
.proto-select {
  padding: 6px 11px;
  border-radius: var(--r-sm);
  border: 1px solid var(--border-weak);
  background: var(--bg-weak);
  font-size: 12px;
  font-family: inherit;
  color: var(--text-strong);
  min-width: 110px;
  cursor: pointer;
  transition: var(--transition);
  outline: none;
}
.proto-select:focus {
  border-color: var(--text-strong);
  box-shadow: 0 0 0 3px var(--bg-interactive);
}
.model-select {
  padding: 6px 11px;
  border-radius: var(--r-sm);
  border: 1px solid var(--border-weak);
  background: var(--bg-weak);
  font-size: 12px;
  font-family: inherit;
  color: var(--text-strong);
  min-width: 200px;
  cursor: pointer;
  transition: var(--transition);
  outline: none;
}
.model-select:focus {
  border-color: var(--text-strong);
  box-shadow: 0 0 0 3px var(--bg-interactive);
}
.btn.sm {
  padding: 5px 10px;
  font-size: 12px;
}

.notice {
  background: var(--bg-weak);
  border: 1px solid var(--border-weak);
  border-radius: var(--r-md);
  padding: 10px 14px;
  color: var(--text-weak);
  font-size: 12px;
  margin-bottom: 14px;
}

/* 弹窗 */
.dialog-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.35);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}
.dialog-box {
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: var(--r-lg);
  padding: 28px 32px 22px;
  min-width: 300px;
  max-width: 420px;
  text-align: center;
}
.dialog-text {
  font-size: 14px;
  color: var(--text-strong);
  margin-bottom: 20px;
  line-height: 1.6;
}

/* 消息列表 */
.messages {
  flex: 1;
  overflow-y: auto;
  padding: 6px 2px 14px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.empty-hint {
  text-align: center;
  color: var(--text-weak);
  padding: 60px 0;
  font-size: 13px;
}
.msg-row {
  display: flex;
  gap: 9px;
  max-width: 82%;
}
.msg-row.user {
  align-self: flex-end;
  flex-direction: row-reverse;
}
.msg-row.assistant {
  align-self: flex-start;
}
.msg-avatar {
  width: 26px;
  height: 26px;
  border-radius: var(--r-sm);
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 10px;
  font-weight: 600;
  flex-shrink: 0;
  border: 1px solid var(--border-weak);
  background: var(--bg-weak);
  color: var(--text-weak);
}
.msg-row.user .msg-avatar {
  background: var(--bg-strong);
  color: var(--text-inverted);
  border-color: var(--bg-strong);
}
.msg-bubble {
  background: var(--bg-weak);
  border: 1px solid var(--border-weak);
  border-radius: var(--r-md);
  padding: 11px 15px;
  font-size: 13px;
  line-height: 1.7;
  word-break: break-word;
}
.msg-row.user .msg-bubble {
  background: var(--bg-strong);
  color: var(--text-inverted);
  border-color: var(--bg-strong);
}

/* 思考过程 */
.thinking-block {
  margin-bottom: 8px;
  border: 1px solid var(--border-weak);
  border-radius: var(--r-sm);
  background: var(--bg);
  overflow: hidden;
}
.thinking-header {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 5px 10px;
  font-size: 9px;
  color: var(--text-weak);
  font-weight: 600;
  letter-spacing: 0.12em;
  border-bottom: 1px solid var(--border-weak);
  background: var(--bg-weak);
}
.thinking-label {
  color: var(--text-weak);
}
.thinking-dots {
  display: inline-flex;
  gap: 3px;
  align-items: center;
}
.thinking-dots span {
  width: 4px;
  height: 4px;
  border-radius: 50%;
  background: var(--text-strong);
  animation: thinking-blink 1.4s infinite ease-in-out both;
}
.thinking-dots span:nth-child(1) {
  animation-delay: 0s;
}
.thinking-dots span:nth-child(2) {
  animation-delay: 0.2s;
}
.thinking-dots span:nth-child(3) {
  animation-delay: 0.4s;
}
@keyframes thinking-blink {
  0%,
  80%,
  100% {
    opacity: 0.2;
    transform: scale(0.7);
  }
  40% {
    opacity: 1;
    transform: scale(1);
  }
}
.thinking-content {
  padding: 7px 11px 9px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--text-weak);
  white-space: pre-wrap;
  font-style: italic;
}

.msg-content {
  white-space: pre-wrap;
}
.msg-error {
  color: var(--red);
  font-size: 12px;
}

/* 等待动画 */
.msg-waiting {
  padding: 4px 0;
}
.dots {
  display: inline-flex;
  gap: 4px;
}
.dots span {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: var(--text-weak);
  animation: bounce 1.4s infinite ease-in-out;
}
.dots span:nth-child(1) {
  animation-delay: -0.32s;
}
.dots span:nth-child(2) {
  animation-delay: -0.16s;
}
@keyframes bounce {
  0%,
  80%,
  100% {
    transform: scale(0.5);
    opacity: 0.4;
  }
  40% {
    transform: scale(1);
    opacity: 1;
  }
}

/* 输入区 */
.input-area {
  display: flex;
  gap: 9px;
  align-items: flex-end;
  padding: 10px 0 0;
  flex-shrink: 0;
}
.input-box {
  flex: 1;
  resize: none;
  padding: 10px 13px;
  border-radius: var(--r-md);
  border: 1px solid var(--border-weak);
  background: var(--bg-weak);
  font-size: 13px;
  font-family: inherit;
  color: var(--text-strong);
  line-height: 1.5;
  transition: var(--transition);
  outline: none;
}
.input-box:focus {
  background: var(--bg-interactive-weaker);
  border-color: var(--text-strong);
  box-shadow: 0 0 0 3px var(--bg-interactive);
}
.input-box:disabled {
  opacity: 0.5;
}
.send-btn {
  height: 42px;
}
</style>
