<script setup lang="ts">
import { commands } from '@/bindings'
import { clearPowerChart } from '@/composables/usePower'
import { usePreference } from '@/stores/preference'
import { useI18n } from 'vue-i18n'

const preference = usePreference()
const tab = useTab()
const { t } = useI18n()
const saving = ref(false)
const clearing = ref(false)
const chartActionError = ref('')

async function saveChart() {
  saving.value = true
  chartActionError.value = ''
  try {
    const result = await commands.saveCurrentChart(tab.value)
    if (result.status === 'error')
      chartActionError.value = `${t('chart.save_failed')}: ${result.error}`
  }
  catch (error) {
    chartActionError.value = `${t('chart.save_failed')}: ${String(error)}`
  }
  finally {
    saving.value = false
  }
}

async function clearChart() {
  clearing.value = true
  chartActionError.value = ''
  try {
    const result = await clearPowerChart(tab.value)
    if (result.status === 'error')
      chartActionError.value = `${t('chart.clear_failed')}: ${result.error}`
  }
  catch (error) {
    chartActionError.value = `${t('chart.clear_failed')}: ${String(error)}`
  }
  finally {
    clearing.value = false
  }
}
</script>

<template>
  <div class="flex flex-col gap-4 min-w-min pb-2 px-4 pt-2">
    <div class="flex gap-6">
      <PowerStatus />
      <PowerFlow />
    </div>
    <PowerUsageChart
      v-if="preference.showPowerUsageChart"
      :saving="saving"
      :clearing="clearing"
      :error="chartActionError"
      @save="saveChart"
      @clear="clearChart"
    />
    <TechnicalDetail />
  </div>
</template>
