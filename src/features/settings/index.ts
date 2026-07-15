import { ref, onMounted } from 'vue'
import {
  getConfig,
  saveConfig,
  enableAutostart,
  disableAutostart,
  autostartStatus,
  type Config
} from '../../api/commands'

export function useSettings() {
  const cfg = ref<Config | null>(null)
  const saving = ref(false)
  const autoStart = ref(false)
  const msg = ref('')
  const keyInput = ref('')

  function addKey() {
    if (!cfg.value) return
    const v = keyInput.value.trim()
    if (!v) return
    cfg.value.consumer.api_keys.push(v)
    keyInput.value = ''
  }
  function removeKey(i: number) {
    if (!cfg.value) return
    cfg.value.consumer.api_keys.splice(i, 1)
  }
  function maskKey(key: string): string {
    if (key.length <= 12) return key.slice(0, 4) + '**'
    return key.slice(0, 6) + '**' + key.slice(-6)
  }

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
      setTimeout(() => {
        msg.value = ''
      }, 3000)
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

  return { cfg, saving, autoStart, msg, keyInput, addKey, removeKey, maskKey, save, toggleAutostart }
}
