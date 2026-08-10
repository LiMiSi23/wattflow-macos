<script setup lang="tsx">
import { Battery, CloudLightningIcon, Cpu, Laptop, Monitor, Smartphone } from 'lucide-vue-next'
import CommonTooltip from './CommonTooltip.vue'

const formatter = new Intl.NumberFormat('en-US', {
  maximumFractionDigits: 1,
  minimumFractionDigits: 1,
})

interface FlowItemProps {
  tooltip: string
  icon: Component
  color: string
}

const colorMap = {
  'text-yellow-500': 'text-yellow-950 dark:text-yellow-50 hover:bg-yellow-500/5 hover:border-yellow-500/20',
  'text-blue-500': 'text-blue-950 dark:text-blue-50 hover:bg-blue-500/5 hover:border-blue-500/20',
  'text-cyan-500': 'text-cyan-950 dark:text-cyan-50 hover:bg-cyan-500/5 hover:border-cyan-500/20',
  'text-indigo-500': 'text-indigo-950 dark:text-indigo-50 hover:bg-indigo-500/5 hover:border-indigo-500/20',
}

const flowArrowClass = `
  relative rounded-full mx-2 w-full overflow-visible
  [--base-color:theme(colors.blue.500)]
  [--base-gradient-color:theme(colors.blue.300)]
  dark:[--base-color:theme(colors.blue.700)]
  dark:[--base-gradient-color:theme(colors.blue.400)]
`

const FlowItem: Component = ({ tooltip, icon, color }: FlowItemProps, { slots }) => {
  return (
    <CommonTooltip content={tooltip} as-child>
      <div
        class={`
        w-24 shrink-0 flex justify-center items-center gap-2
        rounded-lg border bg-background px-2 py-1.5 cursor-pointer
        transition-colors ${colorMap[color]}`}
      >
        { h(icon, { class: `h-4 w-4 ${color}` }) }
        <span class="text-xs font-medium">
          { slots.default?.() }
          <span class="ml-[1px]">w</span>
        </span>
      </div>
    </CommonTooltip>
  )
}
const power = usePower()
const preference = usePreference()
</script>

<template>
  <Card class="flex-1">
    <CardHeader>
      <CardTitle>{{ $t('power_flow') }}</CardTitle>
    </CardHeader>
    <CardContent>
      <Skeleton v-if="power.isLoading" class="w-full h-[120px]" />
      <div
        v-else
        class="flex justify-between items-center w-full rounded-lg border bg-muted/50 p-4 font-mono text-secondary-foreground text-xs h-[120px]"
        :class="[power.isCharging ? '' : 'flex-row-reverse']"
      >
        <FlowItem
          v-if="power.isCharging"
          :tooltip="$t('flow.adapter_power')"
          :icon="CloudLightningIcon"
          color="text-yellow-500"
        >
          {{ formatter.format(power.systemIn + power.efficiencyLoss / 1000) }}
        </FlowItem>

        <CommonTooltip
          v-if="power.isCharging"
          :content="`${$t('flow.power_loss')}: ${power.efficiencyLoss}mw`"
          as-child
        >
          <Shimmer
            :repeat-delay="1500"
            :class="flowArrowClass"
          >
            <div class="h-1 cursor-pointer" />
            <span
              aria-hidden="true"
              class="pointer-events-none absolute -right-1 top-1/2 h-0 w-0 -translate-y-1/2 border-y-[6px] border-y-transparent border-l-[9px] border-l-blue-500 dark:border-l-blue-700"
            />
          </Shimmer>
        </CommonTooltip>

        <div class="flex flex-col items-center gap-2 bg-muted/50 rounded-lg border p-2">
          <FlowItem
            :tooltip="$t('flow.system_total')"
            :icon="power.isRemote ? Smartphone : Laptop"
            color="text-cyan-500"
          >
            {{ formatter.format(power.systemLoad) }}
          </FlowItem>

          <div
            v-if="!power.isRemote && (preference.showScreenPower || preference.showHeatpipePower)"
            class="flex gap-2 rounded-md border bg-background/70 p-1.5"
          >
            <FlowItem
              v-if="preference.showScreenPower"
              :tooltip="power.brightnessPowerAvailable ? $t('flow.screen_power') : $t('flow.screen_power_unavailable')"
              :icon="Monitor"
              color="text-blue-500"
            >
              {{ power.brightnessPowerAvailable ? formatter.format(power.brightnessPower) : '*' }}
            </FlowItem>

            <FlowItem
              v-if="preference.showHeatpipePower"
              :tooltip="power.heatpipePowerAvailable ? $t('flow.heatpipe_power') : $t('flow.soc_power_unavailable')"
              :icon="Cpu"
              color="text-indigo-500"
            >
              {{ power.heatpipePowerAvailable ? formatter.format(power.heatpipePower) : '*' }}
            </FlowItem>
          </div>
        </div>

        <Shimmer
          :delay="2000"
          :repeat-delay="1500"
          :class="flowArrowClass"
        >
          <div class="h-1 cursor-pointer" />
          <span
            aria-hidden="true"
            class="pointer-events-none absolute -right-1 top-1/2 h-0 w-0 -translate-y-1/2 border-y-[6px] border-y-transparent border-l-[9px] border-l-blue-500 dark:border-l-blue-700"
          />
        </Shimmer>

        <FlowItem :tooltip="power.isCharging ? $t('flow.battery_in') : $t('flow.battery_out')" :icon="Battery" color="text-blue-500">
          {{ formatter.format(power.batteryPower) }}
        </FlowItem>
      </div>
    </CardContent>
  </Card>
</template>
