<script setup lang="ts">
import { useSettings } from './index'

const {
  cfg,
  saving,
  autoStart,
  keyInput,
  addKey,
  removeKey,
  save,
  toggleAutostart,
  mapInputs,
  addMapping,
  removeMapping,
  addMapModel,
  removeMapModel,
  isDuplicateAlias,
  isLastUsed
} = useSettings()
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
        <input v-model="cfg.listen" type="text" placeholder="127.0.0.1:8849" />
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
      <div class="row">
        <label>网关自动恢复</label>
        <div class="inline-toggle">
          <label class="toggle">
            <input v-model="cfg.auto_start_gateway" type="checkbox" />
            <span class="slider"></span>
          </label>
          <span>启动应用时恢复上次网关状态</span>
        </div>
      </div>
    </div>

    <!-- Consumer -->
    <div class="group">
      <h3><span class="gn">02</span>Consumer</h3>
      <div class="row">
        <label>API Keys</label>
        <div class="chip-input">
          <span v-for="(k, ki) in cfg.consumer.api_keys" :key="ki" class="chip">
            <span class="chip-text">{{ k }}</span>
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

    <!-- 模型映射 -->
    <div class="group">
      <h3><span class="gn">03</span>模型映射</h3>
      <p class="group-desc">
        用户请求「<b>别名</b>」时，重定向到一组「<b>实际后端模型</b>」（负载均衡 /
        故障转移）。一个别名可映射多个后端，多个别名也可共享同一后端。<br />
        <span class="legend"
          ><i class="legend-dot"></i>蓝色 = 上次成功响应的模型（下次该别名优先使用）</span
        >
      </p>

      <div v-if="!cfg.model_mappings.length" class="map-empty">暂无映射规则</div>

      <div
        v-for="(mm, i) in cfg.model_mappings"
        :key="i"
        class="map-rule"
        :class="{ off: !mm.enabled }"
      >
        <div class="map-rule-head">
          <div class="rh-left">
            <label class="toggle">
              <input v-model="mm.enabled" type="checkbox" />
              <span class="slider"></span>
            </label>
            <span class="rh-title">映射规则 {{ String(i + 1).padStart(2, '0') }}</span>
          </div>
          <button class="btn btn-secondary sm" @click="removeMapping(i)">删除</button>
        </div>
        <div class="map-rule-body">
          <div class="map-side map-alias">
            <span class="mside-lab">请求模型（别名）</span>
            <input v-model="mm.alias" type="text" class="map-inp" placeholder="如 gpt-4" />
          </div>
          <div class="map-arrow">→</div>
          <div class="map-side map-pool">
            <span class="mside-lab">实际模型（后端池）</span>
            <div class="chip-input map-chips">
              <span
                v-for="(m, mi) in mm.models"
                :key="mi"
                class="chip"
                :class="{ 'chip-active': isLastUsed(mm.alias, m) }"
                :title="isLastUsed(mm.alias, m) ? '上次成功响应的模型' : ''"
              >
                <span class="chip-text">{{ m }}</span>
                <button class="chip-x" @click="removeMapModel(i, mi)">×</button>
              </span>
              <input
                v-model="mapInputs[i]"
                class="chip-field"
                :placeholder="mm.models.length ? '' : '实际模型名，回车添加'"
                @keydown.enter.prevent="addMapModel(i)"
              />
            </div>
          </div>
        </div>
        <div v-if="isDuplicateAlias(mm.alias)" class="map-warn">
          ⚠ 别名「{{ mm.alias.trim() }}」在多条规则中重复定义，仅首条生效
        </div>
      </div>

      <div class="map-add">
        <button class="btn btn-secondary sm" @click="addMapping">+ 添加映射规则</button>
        <span class="map-hint">同名别名被多条规则匹配时，仅首条生效</span>
      </div>
    </div>

    <!-- 系统 -->
    <div class="group">
      <h3><span class="gn">04</span>系统</h3>
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
  </div>
</template>

<style src="./index.css" scoped></style>
