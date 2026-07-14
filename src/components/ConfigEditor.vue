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
          <option>info</option><option>debug</option><option>warn</option><option>error</option>
        </select>
      </div>
    </div>

    <!-- Consumer -->
    <div class="group">
      <h3><span class="gn">02</span>Consumer</h3>
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
      <h3><span class="gn">03</span>系统</h3>
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
.page-title { font-size: 20px; font-weight: 600; color: var(--text-strong); letter-spacing: -.01em; }
.page-sub { font-size: 12px; color: var(--text-weak); margin-top: 2px; margin-bottom: 22px; }

/* 分组 — 1px 描边面板 */
.group {
  background: var(--bg); border: 1px solid var(--border-weak); border-radius: var(--r-md);
  padding: 18px 22px; margin-bottom: 12px;
}
.group h3 { font-size: 13px; font-weight: 600; color: var(--text-strong); margin-bottom: 14px; display: flex; align-items: center; gap: 8px; }
.group h3 .gn { font-size: 9px; color: var(--text-weak); border: 1px solid var(--border-weak); border-radius: var(--r-sm); padding: 1px 5px; }
.row {
  display: grid; grid-template-columns: 140px 1fr; gap: 12px;
  align-items: center; margin-bottom: 11px;
}
.row:last-child { margin-bottom: 0; }
.row label { font-size: 12px; color: var(--text); font-weight: 400; }
.row input, .row select {
  background: var(--bg-weak); border: 1px solid var(--border-weak); border-radius: var(--r-md);
  padding: 8px 12px; color: var(--text-strong); font-size: 12px; outline: none;
  font-family: inherit; transition: var(--transition); max-width: 400px;
}
.row input:focus, .row select:focus {
  background: var(--bg-interactive-weaker); border-color: var(--text-strong);
  box-shadow: 0 0 0 3px var(--bg-interactive);
}
.inline-toggle { display: flex; align-items: center; gap: 9px; }
.inline-toggle span { font-size: 12px; color: var(--text); }
.auto-models { max-width: 500px; }
.model-tags { display: flex; flex-wrap: wrap; gap: 4px; }
.model-tag {
  font-size: 10px; padding: 2px 8px; border: 1px solid var(--border-weak);
  border-radius: var(--r-sm); color: var(--text);
}
.muted { font-size: 11px; color: var(--text-weak); font-style: italic; }
.actions { display: flex; gap: 10px; justify-content: flex-end; align-items: center; margin-top: 16px; }
.msg { font-size: 12px; color: var(--text-weak); }
.msg.ok { color: var(--green); }
</style>
