<script setup lang="ts">
import type { ChargingHistory, ChargingHistoryDetail } from '@/bindings'
import { commands } from '@/bindings'
import CustomChartTooltip from '@/components/chart/CustomChartTooltip.vue'
import { useHistory } from '@/composables/useHistory'
import { formatChargingDuration } from '@/lib/format'
import { save } from '@tauri-apps/plugin-dialog'
import { create } from '@tauri-apps/plugin-fs'
import { error as logerror } from '@tauri-apps/plugin-log'
import { format } from 'date-fns'
import { Download, EllipsisVertical, Loader2, Trash2 } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'

const props = defineProps<ChargingHistory>()
const { selectedItem, history } = useHistory()
const { t } = useI18n()

const batteryChangeRate = computed(() => {
  if (props.chargingTime <= 0)
    return null
  return ((props.endLevel - props.fromLevel) / props.chargingTime * 60).toFixed(2)
})

const isLoading = ref(true)
const error = ref()
const data = asyncComputed(
  () => commands.getDetailById(props.id)
    .then((r) => {
      if (r.status === 'error') {
        error.value = r.error
        logerror(r.error)
        return {} as ChargingHistoryDetail
      }
      return r.data
    }),
  {} as ChargingHistoryDetail,
  isLoading,
)

async function exportData() {
  const path = await save({
    title: t('history.export_data'),
    filters: [
      {
        name: t('history.export_filter'),
        extensions: ['json'],
      },
    ],
  })

  if (path) {
    const file = await create(path)
    await file.write(new TextEncoder().encode(JSON.stringify(data.value)))
    await file.close()
  }
}

async function deleteHistory() {
  const result = await commands.deleteHistoryById(props.id)
  if (result.status === 'error') {
    error.value = result.error
    logerror(result.error)
    return
  }
  selectedItem.value = null
  await history.update()
}
</script>

<template>
  <div class="h-full overflow-y-auto">
    <div v-if="isLoading" class="w-full h-full flex items-center justify-center">
      <Loader2 class="animate-spin" />
    </div>
    <div v-else-if="error" class="w-full h-full flex items-center justify-center text-red-500">
      {{ error }}
    </div>
    <div v-else class="px-6 pb-8">
      <div class="flex justify-between items-center">
        <div>
          <h1 class="text-2xl font-bold">
            {{ name || $t('history.unknown_device') }}
          </h1>
          <h2 class="text-sm font-bold mt-1 text-muted-foreground">
            {{ $t('history.adapter', { name: adapterName || $t('history.unknown_adapter') }) }}
          </h2>
        </div>
        <div>
          <DropdownMenu>
            <DropdownMenuTrigger class="p-2 rounded-md hover:bg-muted transition-colors">
              <EllipsisVertical class="w-4 h-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent
              :side-offset="10"
              align="end"
            >
              <DropdownMenuItem
                @click="exportData"
              >
                <Download class="w-4 h-4" />
                {{ $t('history.export_data') }}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem
                class="text-red-500 focus:text-red-500 focus:bg-red-500/10"
                @click="deleteHistory"
              >
                <Trash2 />
                {{ $t('history.delete') }}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
      <div class="mt-4 grid gap-4 grid-cols-3">
        <div class="space-y-2">
          <div class="text-sm font-medium text-muted-foreground">
            {{ $t('history.duration') }}
          </div>
          <div class="text-2xl font-bold">
            {{ formatChargingDuration(chargingTime, t) }}
          </div>
          <div class="text-xs text-muted-foreground">
            {{ format(timestamp * 1000, 'yyyy-MM-dd HH:mm') }}
          </div>
        </div>
        <div class="space-y-2">
          <div class="text-sm font-medium text-muted-foreground">
            {{ $t('history.average_power') }}
          </div>
          <div class="text-2xl font-bold">
            {{ data.avg.systemLoad.toFixed(1) }}W
          </div>
          <div class="text-xs text-muted-foreground">
            {{ $t('history.peak') }}: {{ data.peak.systemLoad.toFixed(1) }}W
          </div>
        </div>
        <div class="space-y-2">
          <div class="text-sm font-medium text-muted-foreground">
            {{ $t('history.battery_change_rate') }}
          </div>
          <div class="text-2xl font-bold">
            {{ batteryChangeRate === null
              ? $t('history.unavailable')
              : $t('history.percent_per_minute', { value: batteryChangeRate }) }}
          </div>
          <div class="text-xs text-muted-foreground">
            {{ $t('history.average_temperature') }}: {{ data.avg.temperature.toFixed(1) }}°C
          </div>
        </div>
      </div>

      <h2 class="mt-8 font-bold">
        {{ $t('history.power_curve') }}
      </h2>
      <LineChart
        class="mt-8 max-h-[220px]"
        index="lastUpdate"
        :data="data.curve.map(d => ({ ...d, lastUpdate: new Date(d.lastUpdate * 1000).toLocaleTimeString() }))"
        :categories="['systemIn', 'batteryPower', 'systemLoad']"
        :custom-tooltip="CustomChartTooltip"
        :show-legend="false"
      />

      <h2 class="mt-8 font-bold">
        {{ $t('history.additional_details') }}
      </h2>
      <div class="mt-2 grid gap-4 text-sm">
        <div class="grid grid-cols-2 gap-4">
          <div>
            <div class="text-muted-foreground">
              {{ $t('history.peak_temperature') }}
            </div>
            <div>{{ data.peak.temperature.toFixed(1) }}°C</div>
          </div>
          <div>
            <div class="text-muted-foreground">
              {{ $t('history.peak_adapter_power') }}
            </div>
            <div>{{ data.peak.adapterPower.toFixed(1) }}W</div>
          </div>
          <div>
            <div class="text-muted-foreground">
              {{ $t('history.adapter_rating') }}
            </div>
            <div>{{ data.peak.adapterWatts }}W({{ data.peak.adapterVoltage }}V, {{ data.peak.adapterAmperage }}A)</div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
