<script setup lang="ts">
import { useProviderUsage } from './index'
import { type GatewayStatus } from '../../api/commands'

defineProps<{ status: GatewayStatus }>()

const {
  config,
  summary,
  selectedProvider,
  selectedKey,
  selectedDays,
  providerKeys,
  loading,
  loadUsage,
  fmtNum,
  maskKey,
  colorForModel
} = useProviderUsage()
</script>

<template>
  <div class="usage-root">
    <h1 class="page-title">供量统计</h1>
    <p class="page-sub">Provider API Key 各模型 Token 使用量</p>

    <!-- 筛选栏 -->
    <div class="usage-filter">
      <div class="uf-item">
        <span class="uf-label">供应商</span>
        <select v-model="selectedProvider" class="select">
          <option :value="0">全部</option>
          <option v-for="p in config?.providers ?? []" :key="p.id" :value="p.id">
            {{ p.name }}
          </option>
        </select>
      </div>
      <div class="uf-item">
        <span class="uf-label">API Key</span>
        <select v-model="selectedKey" class="select">
          <option value="all">全部 Keys</option>
          <option v-for="(key, idx) in providerKeys" :key="idx" :value="key">
            #{{ idx + 1 }} {{ maskKey(key) }}
          </option>
        </select>
      </div>
      <div class="uf-item">
        <span class="uf-label">时间范围</span>
        <div class="seg">
          <input id="pr-1d" v-model.number="selectedDays" type="radio" :value="1" />
          <label for="pr-1d">今天</label>
          <input id="pr-7d" v-model.number="selectedDays" type="radio" :value="7" />
          <label for="pr-7d">7 天</label>
          <input id="pr-30d" v-model.number="selectedDays" type="radio" :value="30" />
          <label for="pr-30d">30 天</label>
          <input id="pr-all" v-model.number="selectedDays" type="radio" :value="0" />
          <label for="pr-all">全部</label>
        </div>
      </div>
    </div>

    <!-- 汇总卡片 -->
    <div class="stats">
      <div class="stat-card">
        <span class="label">总请求数</span>
        <span class="value mono">{{ summary ? fmtNum(summary.total_requests) : '—' }}</span>
      </div>
      <div class="stat-card">
        <span class="label">总 Token</span>
        <span class="value mono">{{ summary ? fmtNum(summary.total_tokens) : '—' }}</span>
      </div>
      <div class="stat-card">
        <span class="label">输入 Token</span>
        <span class="value mono">{{ summary ? fmtNum(summary.total_input_tokens) : '—' }}</span>
      </div>
      <div class="stat-card">
        <span class="label">输出 Token</span>
        <span class="value mono">{{ summary ? fmtNum(summary.total_output_tokens) : '—' }}</span>
      </div>
    </div>

    <!-- 模型明细 -->
    <div class="usage-head">
      <h3>
        模型明细 <span class="ct">{{ summary?.models.length ?? 0 }} 个模型</span>
      </h3>
      <button class="btn btn-secondary sm" :disabled="loading" @click="loadUsage">
        {{ loading ? '加载中...' : '刷新' }}
      </button>
    </div>

    <div v-if="summary && summary.models.length === 0" class="usage-empty">暂无使用记录</div>

    <div v-else-if="summary" class="usage-table">
      <div class="usage-thead">
        <span>模型</span>
        <span class="num">占比</span>
        <span class="num">请求数</span>
        <span class="num">输入</span>
        <span class="num">输出</span>
        <span class="num">合计</span>
      </div>
      <div v-for="(m, i) in summary.models" :key="m.model" class="urow">
        <span class="mdl">
          <span class="mdot" :style="{ background: colorForModel(i) }"></span>
          {{ m.model }}
        </span>
        <span class="num">
          {{ ((m.total_tokens / (summary.total_tokens || 1)) * 100).toFixed(0) }}%
        </span>
        <span class="num">{{ m.requests }}</span>
        <span class="num">{{ fmtNum(m.input_tokens) }}</span>
        <span class="num">{{ fmtNum(m.output_tokens) }}</span>
        <span class="num total">{{ fmtNum(m.total_tokens) }}</span>
      </div>
    </div>
  </div>
</template>

<style src="../usage/index.css" scoped></style>
