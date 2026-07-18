
const nav = document.getElementById('nav');
nav?.addEventListener('click', e => {
  const item = e.target.closest('.nav-item');
  if (!item) return;
  const tab = item.dataset.tab;
  nav.querySelectorAll('.nav-item').forEach(n => n.classList.toggle('active', n === item));
  const panesAll = document.querySelectorAll('.tabpane');
  if (panesAll.length > 1) panesAll.forEach(p => p.classList.toggle('active', p.dataset.pane === tab));
});

const modal = document.getElementById('provider-modal');
const btnAdd = document.getElementById('btn-add-provider');
const btnClose = document.getElementById('modal-close');
const btnCancel = document.getElementById('modal-cancel');
function openModal(){ modal?.classList.add('show'); }
function closeModal(){ modal?.classList.remove('show'); }
btnAdd?.addEventListener('click', openModal);
btnClose?.addEventListener('click', closeModal);
btnCancel?.addEventListener('click', closeModal);
modal?.addEventListener('click', e => { if (e.target === modal) closeModal(); });
document.addEventListener('keydown', e => { if (e.key === 'Escape') closeModal(); });

/* 供应商卡片拖拽排序（纯鼠标事件，与真实应用一致） */
const pcGrid = document.getElementById('pc-grid');
let dragIdx = -1, dragOverIdx = -1, pendingIdx = -1, sx = 0, sy = 0;
function findCardAtPoint(x, y) {
  const el = document.elementFromPoint(x, y);
  if (!el) return -1;
  const card = el.closest('.pc');
  if (!card || !pcGrid.contains(card)) return -1;
  return [...pcGrid.children].indexOf(card);
}
function clearDragState() {
  if (dragIdx !== -1) pcGrid.children[dragIdx]?.classList.remove('dragging');
  if (dragOverIdx !== -1) pcGrid.children[dragOverIdx]?.classList.remove('drag-over');
  pcGrid.classList.remove('dragging');
  dragIdx = dragOverIdx = pendingIdx = -1;
  document.removeEventListener('mousemove', onDragMove);
  document.removeEventListener('mouseup', onDragUp);
}
function onDragMove(e) {
  if (pendingIdx !== -1) {
    const dx = e.clientX - sx, dy = e.clientY - sy;
    if (dx * dx + dy * dy < 16) return;
    dragIdx = pendingIdx; pendingIdx = -1;
    pcGrid.classList.add('dragging');
    pcGrid.children[dragIdx]?.classList.add('dragging');
  }
  if (dragIdx === -1) return;
  e.preventDefault();
  const newOver = findCardAtPoint(e.clientX, e.clientY);
  if (newOver !== dragOverIdx) {
    if (dragOverIdx !== -1) pcGrid.children[dragOverIdx]?.classList.remove('drag-over');
    dragOverIdx = newOver;
    if (dragOverIdx !== -1 && dragOverIdx !== dragIdx) pcGrid.children[dragOverIdx]?.classList.add('drag-over');
  }
}
function onDragUp() {
  if (dragIdx !== -1 && dragOverIdx !== -1 && dragIdx !== dragOverIdx) {
    const moved = pcGrid.children[dragIdx];
    const target = pcGrid.children[dragOverIdx];
    pcGrid.insertBefore(moved, dragOverIdx > dragIdx ? target.nextSibling : target);
  }
  clearDragState();
}
pcGrid?.addEventListener('mousedown', e => {
  const handle = e.target.closest('.drag-handle');
  if (!handle) return;
  if (e.button !== 0) return;
  e.preventDefault();
  const card = handle.closest('.pc');
  pendingIdx = [...pcGrid.children].indexOf(card);
  sx = e.clientX; sy = e.clientY;
  document.addEventListener('mousemove', onDragMove);
  document.addEventListener('mouseup', onDragUp);
});

document.querySelectorAll('.chip-input').forEach(ci => {
  const input = ci.querySelector('.chip-field');
  if (input) {
    input.addEventListener('keydown', e => {
      if (e.key === 'Enter' && e.target.value.trim()) {
        e.preventDefault();
        const chip = document.createElement('span');
        chip.className = 'chip';
        chip.innerHTML = `<span class="ctxt">${e.target.value.trim()}</span><button class="cx">×</button>`;
        chip.querySelector('.cx').addEventListener('click', () => chip.remove());
        ci.insertBefore(chip, input);
        e.target.value = '';
      }
    });
  }
});
document.querySelectorAll('.chip .cx').forEach(x => {
  x.addEventListener('click', () => x.closest('.chip').remove());
});

/* 用量统计 — Consumer Key 各模型 Token 使用量（演示数据） */
const usageData = {
  all: {
    summary: { requests: 698, total: 1356000, input: 1053000, output: 303000 },
    models: [
      { name: 'gpt-4o',            requests: 176, input: 352000, output: 115000, total: 467000 },
      { name: 'claude-3-5-sonnet', requests: 89,  input: 210000, output: 67000,  total: 277000 },
      { name: 'o1',                requests: 12,  input: 145000, output: 18000,  total: 163000 },
      { name: 'gpt-4o-mini',       requests: 320, input: 156000, output: 42000,  total: 198000 },
      { name: 'claude-sonnet-4',   requests: 45,  input: 102000, output: 38000,  total: 140000 },
      { name: 'glm-4-plus',        requests: 56,  input: 88000,  output: 23000,  total: 111000 },
    ],
  },
  'sk-con...0001': {
    summary: { requests: 619, total: 1128000, input: 884000, output: 244000 },
    models: [
      { name: 'gpt-4o',            requests: 142, input: 285000, output: 94000, total: 379000 },
      { name: 'claude-3-5-sonnet', requests: 89,  input: 210000, output: 67000, total: 277000 },
      { name: 'o1',                requests: 12,  input: 145000, output: 18000, total: 163000 },
      { name: 'gpt-4o-mini',       requests: 320, input: 156000, output: 42000, total: 198000 },
      { name: 'glm-4-plus',        requests: 56,  input: 88000,  output: 23000, total: 111000 },
    ],
  },
  'sk-con...0002': {
    summary: { requests: 79, total: 228000, input: 169000, output: 59000 },
    models: [
      { name: 'gpt-4o',          requests: 34, input: 67000,  output: 21000, total: 88000 },
      { name: 'claude-sonnet-4', requests: 45, input: 102000, output: 38000, total: 140000 },
    ],
  },
};
/* Provider 维度演示数据，key 格式: "provider" 或 "provider/keyMask" */
const providerUsageData = {
  all: {
    summary: { requests: 698, total: 1356000, input: 1053000, output: 303000 },
    models: [
      { name: 'gpt-4o',            requests: 176, input: 352000, output: 115000, total: 467000 },
      { name: 'claude-3-5-sonnet', requests: 89,  input: 210000, output: 67000,  total: 277000 },
      { name: 'o1',                requests: 12,  input: 145000, output: 18000,  total: 163000 },
      { name: 'gpt-4o-mini',       requests: 320, input: 156000, output: 42000,  total: 198000 },
      { name: 'claude-sonnet-4',   requests: 45,  input: 102000, output: 38000,  total: 140000 },
      { name: 'glm-4-plus',        requests: 56,  input: 88000,  output: 23000,  total: 111000 },
    ],
  },
  openai: {
    summary: { requests: 508, total: 828000, input: 653000, output: 175000 },
    models: [
      { name: 'gpt-4o',      requests: 176, input: 352000, output: 115000, total: 467000 },
      { name: 'o1',          requests: 12,  input: 145000, output: 18000,  total: 163000 },
      { name: 'gpt-4o-mini', requests: 320, input: 156000, output: 42000,  total: 198000 },
    ],
  },
  'openai/sk-prj**7QxZ': {
    summary: { requests: 320, total: 500000, input: 395000, output: 105000 },
    models: [
      { name: 'gpt-4o',      requests: 110, input: 220000, output: 72000, total: 292000 },
      { name: 'o1',          requests: 8,   input: 95000,  output: 12000, total: 107000 },
      { name: 'gpt-4o-mini', requests: 202, input: 80000,  output: 21000, total: 101000 },
    ],
  },
  'openai/sk-prj**3KdW': {
    summary: { requests: 98, total: 150000, input: 120000, output: 30000 },
    models: [
      { name: 'gpt-4o',      requests: 35, input: 70000, output: 23000, total: 93000 },
      { name: 'gpt-4o-mini', requests: 63, input: 50000, output: 7000,  total: 57000 },
    ],
  },
  'openai/sk-prj**9BnL': {
    summary: { requests: 90, total: 178000, input: 138000, output: 40000 },
    models: [
      { name: 'gpt-4o',      requests: 31, input: 62000, output: 20000, total: 82000 },
      { name: 'o1',          requests: 4,  input: 50000, output: 6000,  total: 56000 },
      { name: 'gpt-4o-mini', requests: 55, input: 26000, output: 14000, total: 40000 },
    ],
  },
  anthropic: {
    summary: { requests: 134, total: 417000, input: 312000, output: 105000 },
    models: [
      { name: 'claude-3-5-sonnet', requests: 89, input: 210000, output: 67000, total: 277000 },
      { name: 'claude-sonnet-4',   requests: 45, input: 102000, output: 38000, total: 140000 },
    ],
  },
  'anthropic/sk-ant**4MnK': {
    summary: { requests: 88, total: 270000, input: 202000, output: 68000 },
    models: [
      { name: 'claude-3-5-sonnet', requests: 58, input: 137000, output: 44000, total: 181000 },
      { name: 'claude-sonnet-4',   requests: 30, input: 65000,  output: 24000, total: 89000 },
    ],
  },
  'anthropic/sk-ant**8RtQ': {
    summary: { requests: 46, total: 147000, input: 110000, output: 37000 },
    models: [
      { name: 'claude-3-5-sonnet', requests: 31, input: 73000, output: 23000, total: 96000 },
      { name: 'claude-sonnet-4',   requests: 15, input: 37000, output: 14000, total: 51000 },
    ],
  },
  glm: {
    summary: { requests: 56, total: 111000, input: 88000, output: 23000 },
    models: [
      { name: 'glm-4-plus', requests: 56, input: 88000, output: 23000, total: 111000 },
    ],
  },
  'glm/xxxxxxxx**f2a1': {
    summary: { requests: 56, total: 111000, input: 88000, output: 23000 },
    models: [
      { name: 'glm-4-plus', requests: 56, input: 88000, output: 23000, total: 111000 },
    ],
  },
};
/* 各供应商的 key 列表（用于联动下拉） */
const providerKeys = {
  openai:    ['sk-prj**7QxZ', 'sk-prj**3KdW', 'sk-prj**9BnL'],
  anthropic: ['sk-ant**4MnK', 'sk-ant**8RtQ'],
  glm:       ['xxxxxxxx**f2a1'],
};
const modelColors = {
  'gpt-4o': '#1f1e1e',
  'gpt-4o-mini': '#8c8b8b',
  'claude-3-5-sonnet': '#7c3aed',
  'claude-sonnet-4': '#b394e8',
  'o1': '#03b000',
  'glm-4-plus': '#c0703a',
};
function fmtNum(n) {
  if (n >= 1000000) return (n / 1000000).toFixed(2) + 'M';
  if (n >= 1000) return (n / 1000).toFixed(1) + 'K';
  return String(n);
}
function renderUsage(key) {
  const d = usageData[key];
  if (!d) return;
  document.getElementById('s-req').textContent = fmtNum(d.summary.requests);
  document.getElementById('s-total').textContent = fmtNum(d.summary.total);
  document.getElementById('s-input').textContent = fmtNum(d.summary.input);
  document.getElementById('s-output').textContent = fmtNum(d.summary.output);
  document.getElementById('usage-model-count').textContent = d.models.length + ' 个模型';
  const rowsEl = document.getElementById('usage-rows');
  const emptyEl = document.getElementById('usage-empty');
  if (!d.models.length) { rowsEl.innerHTML = ''; emptyEl.style.display = 'block'; return; }
  emptyEl.style.display = 'none';
  rowsEl.innerHTML = d.models.map(m => {
    const pct = (m.total / d.summary.total * 100).toFixed(0);
    const color = modelColors[m.name] || '#1f1e1e';
    return '<div class="urow">'
      + '<span class="mdl"><span class="mdot" style="background:' + color + '"></span>' + m.name + '</span>'
      + '<span class="num">' + pct + '%</span>'
      + '<span class="num">' + m.requests + '</span>'
      + '<span class="num">' + fmtNum(m.input) + '</span>'
      + '<span class="num">' + fmtNum(m.output) + '</span>'
      + '<span class="num total">' + fmtNum(m.total) + '</span>'
      + '</div>';
  }).join('');
}
if (document.getElementById('usage-key')) renderUsage('all');
document.getElementById('usage-key')?.addEventListener('change', e => renderUsage(e.target.value));

/* 供量统计 — 渲染 + 联动 */
function renderProviderUsage() {
  const provider = document.getElementById('pu-provider').value;
  const key = document.getElementById('pu-key').value;
  const dataKey = key === 'all' ? provider : provider + '/' + key;
  const d = providerUsageData[dataKey] || providerUsageData[provider] || providerUsageData.all;
  document.getElementById('pu-req').textContent = fmtNum(d.summary.requests);
  document.getElementById('pu-total').textContent = fmtNum(d.summary.total);
  document.getElementById('pu-input').textContent = fmtNum(d.summary.input);
  document.getElementById('pu-output').textContent = fmtNum(d.summary.output);
  document.getElementById('pu-model-count').textContent = d.models.length + ' 个模型';
  const rowsEl = document.getElementById('pu-rows');
  const emptyEl = document.getElementById('pu-empty');
  if (!d.models.length) { rowsEl.innerHTML = ''; emptyEl.style.display = 'block'; return; }
  emptyEl.style.display = 'none';
  rowsEl.innerHTML = d.models.map(m => {
    const pct = (m.total / d.summary.total * 100).toFixed(0);
    const color = modelColors[m.name] || '#1f1e1e';
    return '<div class="urow">'
      + '<span class="mdl"><span class="mdot" style="background:' + color + '"></span>' + m.name + '</span>'
      + '<span class="num">' + pct + '%</span>'
      + '<span class="num">' + m.requests + '</span>'
      + '<span class="num">' + fmtNum(m.input) + '</span>'
      + '<span class="num">' + fmtNum(m.output) + '</span>'
      + '<span class="num total">' + fmtNum(m.total) + '</span>'
      + '</div>';
  }).join('');
}
/* 供应商切换时联动 key 下拉 */
document.getElementById('pu-provider')?.addEventListener('change', e => {
  const keySel = document.getElementById('pu-key');
  const p = e.target.value;
  keySel.innerHTML = '<option value="all">全部 Keys</option>';
  if (p !== 'all' && providerKeys[p]) {
    providerKeys[p].forEach(k => {
      const opt = document.createElement('option');
      opt.value = k; opt.textContent = k;
      keySel.appendChild(opt);
    });
  }
  renderProviderUsage();
});
document.getElementById('pu-key')?.addEventListener('change', renderProviderUsage);
if (document.getElementById('pu-provider')) renderProviderUsage();

/* 模型映射 — 别名(单个) → 实际后端池(多个)；别名重复检测 + 增删规则 */
(function(){
  const list = document.getElementById('map-list');
  if(!list) return;
  function aliasOf(rule){ const i = rule.querySelector('.map-alias .inp'); return i ? i.value.trim() : ''; }
  function poolNames(rule){ return Array.from(rule.querySelectorAll('.map-pool .chip .ctxt')).map(c=>c.textContent.trim()).filter(Boolean); }
  /* 重算：别名若被多条规则重复定义 → 提示冲突（仅首条生效） */
  function recompute(){
    const cnt = {};
    list.querySelectorAll('[data-rule]').forEach(r=>{ const a = aliasOf(r); if(a) cnt[a]=(cnt[a]||0)+1; });
    list.querySelectorAll('[data-rule]').forEach(rule=>{
      const a = aliasOf(rule); const dup = a && cnt[a] > 1;
      const warn = rule.querySelector('[data-warn]'); const msg = rule.querySelector('[data-warn-msg]');
      if(!warn) return;
      const txt = dup ? '别名「'+a+'」在多条规则中重复定义，仅首条生效' : '';
      const disp = dup ? 'flex' : 'none';
      if(warn.style.display !== disp) warn.style.display = disp;
      if(msg && msg.textContent !== txt) msg.textContent = txt;   /* 避免触发观察器死循环 */
    });
  }
  /* 后端池 chip 增删 — 捕获阶段接管，避免与全局 chip 处理重复触发 */
  list.addEventListener('keydown', e=>{
    const field = e.target.closest('.map-pool .chip-field'); if(!field) return;
    if(e.key==='Enter' && field.value.trim()){
      e.preventDefault(); e.stopPropagation();
      const ci = field.closest('.chip-input');
      const chip = document.createElement('span'); chip.className='chip';
      chip.innerHTML = '<span class="ctxt">'+field.value.trim()+'</span><button class="cx">×</button>';
      ci.insertBefore(chip, field); field.value='';
    }
  }, true);
  list.addEventListener('click', e=>{
    const x = e.target.closest('.map-pool .chip .cx');
    if(x){ e.stopPropagation(); x.closest('.chip').remove(); return; }
    if(e.target.closest('.map-del') && list.querySelectorAll('[data-rule]').length > 1){
      e.target.closest('[data-rule]').remove();
    }
  }, true);
  list.addEventListener('input', e=>{ if(e.target.closest('.map-alias')) recompute(); });
  new MutationObserver(recompute).observe(list, {childList:true, subtree:true, characterData:true});
  const addBtn = document.getElementById('map-add');
  if(addBtn) addBtn.addEventListener('click', ()=>{
    const clone = list.querySelector('[data-rule]').cloneNode(true);
    clone.querySelectorAll('.chip').forEach(c=>c.remove());
    const inp = clone.querySelector('.map-alias .inp'); if(inp) inp.value='';
    const warn = clone.querySelector('[data-warn]'); if(warn) warn.style.display='none';
    list.appendChild(clone); recompute();
    clone.scrollIntoView({behavior:'smooth', block:'nearest'});
  });
  recompute();
})();

/* ===== OpenCode 配置页交互 ===== */

/* Provider / Model 卡片折叠 */
(function(){
  document.querySelectorAll('.pv-head, .md-row-head').forEach(head => {
    head.addEventListener('click', e => {
      if(e.target.closest('button, input, .toggle, .seg, .chip-input, .mod-tag, .inp, select, textarea, .oc-sel')) return;
      const card = head.closest('.pv-card, .md-row');
      if(card) card.classList.toggle('expanded');
    });
  });
})();

/* modalities 多选 tag 切换 */
(function(){
  document.querySelectorAll('.mod-tag').forEach(tag => {
    tag.addEventListener('click', e => { e.stopPropagation(); tag.classList.toggle('on'); });
  });
})();
