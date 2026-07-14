<script setup lang="ts">
import { ref, onMounted } from 'vue'
import {
  getConfig, saveConfig, enableAutostart, disableAutostart, autostartStatus,
  type Config,
} from '../api/commands'

const cfg = ref<Config | null>(null)
const saving = ref(false)
const autoStart = ref(false)
const msg = ref('')

async function load() {
  try {
    cfg.value = await getConfig()
    autoStart.value = await autostartStatus()
  } catch (e) {
    console.error(e)
  }
}

async function save() {
  if (!cfg.value) return
  saving.value = true
  msg.value = ''
  try {
    await saveConfig(cfg.value)
    msg.value = '配置已保存'
    setTimeout(() => { msg.value = '' }, 3000)
  } catch (e) {
    msg.value = '保存失败: ' + String(e)
  } finally {
    saving.value = false
  }
}

async function toggleAutostart(val: boolean) {
  try {
    if (val) await enableAutostart()
    else await disableAutostart()
    autoStart.value = val
  } catch (e) {
    alert(String(e))
  }
}

onMounted(load)
</script>

<template>
  <div v-if="cfg">
    <h1 class="page-title">设置</h1>
    <p class="page-sub">网关全局配置</p>

    <!-- 通用 -->
    <div class="group">
      <h3>通用</h3>
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
          <option>info</option><option>debug</option><option>warn</option><option>error</option>
        </select>
      </div>
    </div>

    <!-- Consumer -->
    <div class="group">
      <h3>Consumer</h3>
      <div class="row">
        <label>API Keys</label>
        <input :value="cfg.consumer.api_keys.join(', ')"
          @input="cfg.consumer.api_keys = ($event.target as HTMLInputElement).value.split(',').map(s=>s.trim()).filter(Boolean)"
          type="text" placeholder="sk-key1, sk-key2" />
      </div>
      <div class="row">
        <label>Models</label>
        <div class="auto-models">
          <div class="model-tags">
            <span v-for="m in cfg.consumer.models" :key="m" class="model-tag">{{ m }}</span>
            <span v-if="!cfg.consumer.models.length" class="muted">（自动从已启用的提供商聚合）</span>
          </div>
        </div>
      </div>
    </div>

    <!-- 系统 -->
    <div class="group">
      <h3>系统</h3>
      <div class="row">
        <label>开机自启</label>
        <div class="inline-toggle">
          <label class="toggle">
            <input type="checkbox" :checked="autoStart" @change="toggleAutostart(($event.target as HTMLInputElement).checked)" />
            <span class="slider"></span>
          </label>
          <span>系统启动时自动运行</span>
        </div>
      </div>
    </div>

    <!-- 保存按钮 -->
    <div class="actions">
      <span class="msg" :class="{ ok: msg.includes('已保存') }">{{ msg }}</span>
      <button class="btn btn-primary" :disabled="saving" @click="save">
        {{ saving ? '保存中...' : '保存配置' }}
      </button>
    </div>
  </div>
</template>

<style scoped>
.page-title { font-size: 22px; font-weight: 700; margin-bottom: 4px; }
.page-sub { font-size: 13px; color: var(--text-secondary); margin-bottom: 24px; }
.group {
  background: var(--bg-card); border-radius: var(--radius-md);
  border: 1px solid var(--border); padding: 20px 24px; margin-bottom: 16px;
}
.group h3 { font-size: 14px; font-weight: 600; margin-bottom: 16px; }
.row {
  display: grid; grid-template-columns: 140px 1fr; gap: 12px;
  align-items: center; margin-bottom: 12px;
}
.row:last-child { margin-bottom: 0; }
.row label { font-size: 13px; color: var(--text-secondary); font-weight: 500; }
.row input, .row select {
  background: var(--bg-deep); border: 1px solid var(--border);
  border-radius: var(--radius-sm); padding: 8px 12px;
  color: var(--text-primary); font-size: 13px; outline: none;
  font-family: inherit; transition: var(--transition);
  max-width: 400px;
}
.row input:focus, .row select:focus {
  border-color: var(--amber); box-shadow: 0 0 0 2px rgba(37,99,235,.12);
}
.inline-toggle { display: flex; align-items: center; gap: 10px; }
.inline-toggle span { font-size: 13px; color: var(--text-secondary); }
.auto-models { max-width: 500px; }
.model-tags { display: flex; flex-wrap: wrap; gap: 4px; }
.model-tag {
  font-size: 12px; padding: 3px 10px; border-radius: 4px;
  background: var(--bg-elevated); color: var(--text-secondary);
  border: 1px solid var(--border);
}
.muted { font-size: 12px; color: var(--text-muted); font-style: italic; }
.actions { display: flex; gap: 12px; justify-content: flex-end; align-items: center; margin-top: 20px; }
.msg { font-size: 13px; color: var(--text-secondary); }
.msg.ok { color: var(--green); }
</style>
