// ============================================================
// ai·aggregs 界面样式稿 · 交互脚本
// 让静态样式稿可点击：toggle 开关、折叠卡片、modal 等
// ============================================================
;(function () {
  'use strict'

  // ---- Toggle 开关：点击切换 checked 状态 ----
  document.addEventListener('click', function (e) {
    var toggle = e.target.closest('.toggle')
    if (!toggle) return
    // 跳过原生 input 自身已处理的（label 包裹时无需干预）
    var input = toggle.querySelector('input[type="checkbox"]')
    if (input && !e.target.matches('input')) {
      input.checked = !input.checked
      e.preventDefault()
    }
  })

  // ---- Provider / Model 折叠卡片：点击头部展开/收起 ----
  document.addEventListener('click', function (e) {
    var head = e.target.closest('.pv-head, .md-row-head')
    if (!head) return
    // 排除点击内部按钮（删除/添加）
    if (e.target.closest('button')) return
    var card = head.closest('.pv-card, .md-row')
    if (card) {
      card.classList.toggle('expanded')
      e.preventDefault()
    }
  })

  // ---- modal 打开/关闭 ----
  document.addEventListener('click', function (e) {
    // 打开：带 data-open-modal 的按钮
    var opener = e.target.closest('[data-open-modal]')
    if (opener) {
      var sel = opener.getAttribute('data-open-modal')
      var modal = document.querySelector(sel)
      if (modal) {
        modal.classList.add('open')
        e.preventDefault()
      }
      return
    }
    // 关闭：overlay 点击 / 带 data-close 的按钮
    if (e.target.matches('.modal-overlay')) {
      e.target.classList.remove('open')
      return
    }
    var closer = e.target.closest('[data-close]')
    if (closer) {
      var overlay = closer.closest('.modal-overlay')
      if (overlay) overlay.classList.remove('open')
      e.preventDefault()
    }
  })

  // ---- ESC 关闭 modal ----
  document.addEventListener('keydown', function (e) {
    if (e.key !== 'Escape') return
    document.querySelectorAll('.modal-overlay.open').forEach(function (m) {
      m.classList.remove('open')
    })
  })
})()
