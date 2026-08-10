<script setup lang="ts">
import { Loader2, Save, Trash2 } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'
import CustomChartTooltip from './CustomChartTooltip.vue'

const props = withDefaults(defineProps<{
  saving?: boolean
  clearing?: boolean
  error?: string
}>(), {
  saving: false,
  clearing: false,
  error: '',
})

const emit = defineEmits<{
  save: []
  clear: []
}>()

const { t } = useI18n()
const power = usePower()
const preference = usePreference()

const categoryColors: Record<keyof StatisticData, string> = {
  'time': '',
  'System Power': '#2563eb',
  'System In': '#eab308',
  'Screen Power': '#60a5fa',
  'Heatpipe Power': '#818cf8',
}

const categories = computed(() => {
  const base = ['System Power'] as (keyof StatisticData)[]
  if (!power.value.isRemote) {
    if (preference.showScreenPower && power.value.brightnessPowerAvailable)
      base.push('Screen Power')
    if (preference.showHeatpipePower && power.value.heatpipePowerAvailable)
      base.push('Heatpipe Power')
  }
  if (power.value.isCharging) {
    base.push('System In')
  }
  return base
})

const localeMap = computed(() => ({
  'System Power': t('flow.system_total'),
  'Screen Power': t('flow.screen_power'),
  'Heatpipe Power': t('flow.heatpipe_power'),
  'System In': t('flow.system_in'),
}))

const localedData = computed(() => {
  return power.value.statistics.map((item) => {
    return Object.fromEntries(Object.entries(item).map(([key, value]) => [localeMap.value[key] || key, value]))
  })
})

const localedCategories = computed(() => categories.value.map(item => localeMap.value[item] || item))
const colors = computed(() => categories.value.map(item => categoryColors[item]))
const chartKey = computed(() => localedCategories.value.join('|'))
const hasChartPoints = computed(() => power.value.statistics.length > 0)
</script>

<template>
  <Card class="w-full space-y-8 relative">
    <CardHeader class="pb-0 flex-row items-center justify-between space-y-0">
      <CardTitle>
        {{ $t('power_usage') }}
      </CardTitle>
      <div class="flex flex-col items-end gap-1">
        <div class="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            :disabled="!hasChartPoints || props.saving || props.clearing"
            @click="emit('save')"
          >
            <Loader2 v-if="props.saving" class="mr-2 size-4 animate-spin" />
            <Save v-else class="mr-2 size-4" />
            {{ props.saving ? $t('chart.saving') : $t('chart.save') }}
          </Button>
          <Button
            variant="outline"
            size="sm"
            :disabled="!hasChartPoints || props.saving || props.clearing"
            @click="emit('clear')"
          >
            <Loader2 v-if="props.clearing" class="mr-2 size-4 animate-spin" />
            <Trash2 v-else class="mr-2 size-4" />
            {{ props.clearing ? $t('chart.clearing') : $t('chart.clear') }}
          </Button>
        </div>
        <p v-if="props.error" class="max-w-96 text-right text-xs text-red-500">
          {{ props.error }}
        </p>
      </div>
    </CardHeader>
    <CardContent>
      <Skeleton v-if="power.isLoading" class="w-full h-[240px]" />
      <LineChart
        v-else
        :key="chartKey"
        class="w-full h-[240px] font-bold"
        index="time"
        :y-formatter="(value) => `${value}w`"
        :data="localedData"
        :categories="localedCategories"
        :custom-tooltip="CustomChartTooltip"
        :colors="colors"
      />
    </CardContent>
  </Card>
</template>
