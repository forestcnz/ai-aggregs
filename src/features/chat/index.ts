import { ref, nextTick, onMounted, computed } from 'vue'
import { getConfig, type GatewayStatus, type Config } from '../../api/commands'

type ChatProtocol = 'chat' | 'responses' | 'anthropic'

// 模块作用域：跨聊天页实例共享，切页保持上次选择（纯内存，重启应用重置）
const lastProtocol = ref<ChatProtocol>('chat')
const lastModel = ref('')
const lastKey = ref('')

export function useChat(props: { status: GatewayStatus }) {
  // ---- 协议类型 ----
  const PROTOCOL_ENDPOINT: Record<ChatProtocol, string> = {
    chat: '/v1/chat/completions',
    responses: '/v1/responses',
    anthropic: '/v1/messages'
  }

  // ---- 状态 ----
  const config = ref<Config | null>(null)
  const protocol = lastProtocol
  const selectedModel = lastModel
  const selectedKey = lastKey
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
  const models = computed(() =>
    [...(config.value?.consumer.models ?? [])].sort((a, b) => a.localeCompare(b)),
  )
  const apiKeys = computed(() => config.value?.consumer.api_keys ?? [])
  const gatewayUrl = computed(() => {
    if (!props.status.running || !props.status.listen_addr) return ''
    return `http://${props.status.listen_addr}`
  })
  const apiKey = computed(() => selectedKey.value)

  // ---- 生命周期 ----
  onMounted(async () => {
    try {
      config.value = await getConfig()
      // 保持上次选择；仅当为空或已不在列表时回退到首项
      if (models.value.length > 0 && !models.value.includes(selectedModel.value)) {
        selectedModel.value = models.value[0]
      }
      if (apiKeys.value.length > 0 && !apiKeys.value.includes(selectedKey.value)) {
        selectedKey.value = apiKeys.value[0]
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
    if (!selectedKey.value) {
      showDialog('请先选择 API Key')
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

  // ---- 工具：掩码显示 key ----
  function maskKey(k: string): string {
    if (k.length <= 8) return k
    return k.slice(0, 4) + '...' + k.slice(-4)
  }

  return {
    protocol, selectedModel, selectedKey, input, sending, dialogMsg,
    messages, scrollEl, textareaRef, models, apiKeys, maskKey,
    closeDialog, send, stop, clearChat, onKeydown
  }
}
