import type { StatusBarItem, Theme } from '@/bindings'
import { defineStore } from 'pinia'
import { ref } from 'vue'

export const usePreference = defineStore('preference', () => {
  const theme = ref<Theme>('system')
  const animationsEnabled = ref(true)
  const updateInterval = ref(1500)
  const language = ref('en')
  const statusBarItem = ref<StatusBarItem>('system')
  const statusBarShowCharging = ref(true)
  const showScreenPower = ref(true)
  const showHeatpipePower = ref(true)
  const showPowerUsageChart = ref(true)
  const autoSaveChart = ref(false)

  return {
    theme,
    animationsEnabled,
    updateInterval,
    language,
    statusBarItem,
    statusBarShowCharging,
    showScreenPower,
    showHeatpipePower,
    showPowerUsageChart,
    autoSaveChart,
  }
}, {
  tauri: {
    saveOnChange: true,
    saveStrategy: 'debounce',
    saveInterval: 1000,
  },
})

let preferenceStartPromise: Promise<void> | undefined

/**
 * The persistence plugin marks a store as enabled before hydration finishes.
 * Share the first start promise so callers cannot observe the default values
 * while the persisted state is still loading.
 */
export function startPreferenceStore(preference = usePreference()) {
  preferenceStartPromise ??= preference.$tauri.start()
  return preferenceStartPromise
}

export function usePreferenceAsync() {
  const preference = usePreference()
  const isLoading = ref(true)
  startPreferenceStore(preference).then(() => isLoading.value = false)
  return { preference, isLoading }
}
