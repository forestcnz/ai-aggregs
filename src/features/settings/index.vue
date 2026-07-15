<script setup lang="ts">
import { useSettings } from './index'

const { cfg, saving, autoStart, msg, keyInput, addKey, removeKey, maskKey, save, toggleAutostart } = useSettings()
</script>

<template>
  <div v-if="cfg">
    <h1 class="page-title">设置</h1>
    <p class="page-sub">网关全局配置</p>

    <!-- 通用 -->
    <div class="group">
      <h3><span class="gn">01</span>通用</h3>
      <div class="row">
        <label>监听地址</label>
        <input v-model="cfg.listen" type="text" placeholder="127.0.0.1:8000" />
      </div>
      <div class="row">
        <label>Key 黑名单时长(秒)</label>
        <input v-model.number="cfg.key_blacklist_secs" type="number" />
      </div>
      <div class="row">
        <label>日志级别</label>
        <select v-model="cfg.log.level">
          <option>info</option>
          <option>debug</option>
          <option>warn</option>
          <option>error</option>
        </select>
      </div>
    </div>

    <!-- Consumer -->
    <div class="group">
      <h3><span class="gn">02</span>Consumer</h3>
      <div class="row">
        <label>API Keys</label>
        <div class="chip-input">
          <span v-for="(k, ki) in cfg.consumer.api_keys" :key="ki" class="chip">
            <span class="chip-text">{{ maskKey(k) }}</span>
            <button class="chip-x" @click="removeKey(ki)">×</button>
          </span>
          <input
            v-model="keyInput"
            class="chip-field"
            :placeholder="cfg.consumer.api_keys.length ? '' : 'API Key，回车添加'"
            @keydown.enter.prevent="addKey"
          />
        </div>
      </div>
      <div class="row">
        <label>Models</label>
        <div class="auto-models">
          <div class="model-tags">
            <span v-for="m in cfg.consumer.models" :key="m" class="model-tag">{{ m }}</span>
            <span v-if="!cfg.consumer.models.length" class="muted"
              >（自动从已启用的提供商聚合）</span
            >
          </div>
        </div>
      </div>
    </div>

    <!-- 系统 -->
    <div class="group">
      <h3><span class="gn">03</span>系统</h3>
      <div class="row">
        <label>开机自启</label>
        <div class="inline-toggle">
          <label class="toggle">
            <input
              type="checkbox"
              :checked="autoStart"
              @change="toggleAutostart(($event.target as HTMLInputElement).checked)"
            />
            <span class="slider"></span>
          </label>
          <span>系统启动时自动运行</span>
        </div>
      </div>
    </div>

    <!-- 保存按钮 -->
    <div class="actions">
      <button class="btn btn-primary" :disabled="saving" @click="save">
        {{ saving ? '保存中...' : '保存配置' }}
      </button>
    </div>

    <!-- 保存提示 toast -->
    <Transition name="toast">
      <div v-if="msg" class="toast" :class="{ ok: msg.includes('已保存') }">{{ msg }}</div>
    </Transition>
  </div>
</template>

<style src="./index.css" scoped></style>
