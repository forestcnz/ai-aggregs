<script setup lang="ts">
import { useChat } from './index'
import { type GatewayStatus } from '../../api/commands'

// 显式声明组件名，供 App.vue 中 <KeepAlive include="ChatView"> 精确匹配
defineOptions({ name: 'ChatView' })

const props = defineProps<{ status: GatewayStatus }>()
const {
  protocol,
  selectedModel,
  selectedKey,
  input,
  sending,
  messages,
  scrollEl,
  textareaRef,
  models,
  apiKeys,
  send,
  stop,
  clearChat,
  onKeydown,
  refreshConfig
} = useChat(props)
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
        <select v-model="selectedModel" class="model-select" :disabled="sending" @focus="refreshConfig">
          <option v-if="models.length === 0" value="" disabled>无可用模型</option>
          <option v-for="m in models" :key="m" :value="m">{{ m }}</option>
        </select>
        <select v-model="selectedKey" class="key-select" :disabled="sending" @focus="refreshConfig">
          <option v-if="apiKeys.length === 0" value="" disabled>未配置 Key</option>
          <option v-for="k in apiKeys" :key="k" :value="k">{{ k }}</option>
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
      <button v-if="!sending" class="btn btn-primary send-btn" @click="send">发送</button>
      <button v-else class="btn btn-stop send-btn" @click="stop">停止</button>
    </div>
  </div>
</template>

<style src="./index.css" scoped></style>
