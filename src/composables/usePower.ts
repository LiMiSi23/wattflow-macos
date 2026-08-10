import type { ChartPoint, CurrentChart, InterfaceType, NormalizedResource } from '@/bindings'
import type { Reactive } from 'vue'
import { commands, events } from '@/bindings'
import { useDocumentVisibility } from '@vueuse/core'

import { computed, reactive } from 'vue'
import { useTab } from './useTab'

const MAX_STATISTICS_LENGTH = 100

export interface StatisticData {
  'time': string
  'System Power': number
  'System In': number
  'Screen Power'?: number
  'Heatpipe Power'?: number
}

interface RawPowerData {
  data: NormalizedResource
  chartPoints: ChartPoint[]
  statistics: StatisticData[]
  chartSessionId: string
  lastChartSequence: number
}

const localPowerData: Reactive<RawPowerData> = reactive({
  data: {} as NormalizedResource,
  chartPoints: [],
  statistics: [],
  chartSessionId: '',
  lastChartSequence: 0,
})

events.powerTickEvent.listen(({ payload: { data } }) => {
  localPowerData.data = data
})

events.devicePowerTickEvent.listen(({ payload: { data, udid } }) => {
  const deviceData = getOrCreateDeviceData(udid)
  deviceData.data = data
})

export type RemotePowerData = RawPowerData & {
  name: string
  offline: boolean
  interface: Set<InterfaceType>
}

interface PowerData {
  local: RawPowerData
  remote: Record<string, RemotePowerData>
}

const power = reactive<PowerData>({
  local: localPowerData,
  remote: {},
})

const chartStateGenerations = new Map<string, number>()
const chartEventSessionGenerations = new Map<string, number>()
const chartLoadTokens = new Map<string, number>()
const retiredChartSessions = new Map<string, Set<string>>()
let nextChartLoadToken = 0

// Both listeners must be active before requesting a snapshot. Otherwise a
// point/reset emitted during startup can be lost and the UI can show stale data.
const chartListenersReady = Promise.all([
  events.chartPointEvent.listen(({ payload }) => {
    applyChartPoint(payload.deviceId, payload.sessionId, payload.point)
  }),
  events.chartResetEvent.listen(({ payload }) => {
    applyChartReset(payload)
  }),
]).then(() => undefined)

function generationFor(map: Map<string, number>, deviceId: string) {
  return map.get(deviceId) ?? 0
}

function incrementGeneration(map: Map<string, number>, deviceId: string) {
  map.set(deviceId, generationFor(map, deviceId) + 1)
}

function retireChartSession(deviceId: string, sessionId: string) {
  if (!sessionId)
    return

  const retired = retiredChartSessions.get(deviceId) ?? new Set<string>()
  retired.add(sessionId)
  // This only guards against late events from recently replaced sessions.
  // Keeping a small bounded set avoids growing for the lifetime of the app.
  if (retired.size > 32)
    retired.delete(retired.values().next().value!)
  retiredChartSessions.set(deviceId, retired)
}

function isRetiredChartSession(deviceId: string, sessionId: string) {
  return retiredChartSessions.get(deviceId)?.has(sessionId) ?? false
}

function getOrCreateDeviceData(udid: string): RemotePowerData {
  if (!power.remote[udid]) {
    power.remote[udid] = {
      data: {} as NormalizedResource,
      chartPoints: [],
      statistics: [],
      chartSessionId: '',
      lastChartSequence: 0,
      name: '',
      offline: false,
      interface: new Set(),
    }
    void chartListenersReady
      .then(() => loadCurrentChart(udid))
      .catch(error => console.error(error))
  }
  return power.remote[udid]
}

function getChartData(deviceId: string): RawPowerData {
  return deviceId === 'local' ? localPowerData : getOrCreateDeviceData(deviceId)
}

function statisticFromPoint(point: ChartPoint): StatisticData {
  const data = point.data
  const statistic: StatisticData = {
    'time': new Date(point.capturedAt).toLocaleTimeString(),
    'System Power': data.systemLoad,
    'System In': data.systemIn,
  }
  if (data.brightnessPowerAvailable)
    statistic['Screen Power'] = data.brightnessPower
  if (data.heatpipePowerAvailable)
    statistic['Heatpipe Power'] = data.heatpipePower
  return statistic
}

function rebuildStatistics(target: RawPowerData) {
  target.statistics.splice(
    0,
    target.statistics.length,
    ...target.chartPoints.map(statisticFromPoint),
  )
  target.lastChartSequence = target.chartPoints.at(-1)?.sequence ?? 0
}

function mergeChartPoints(target: RawPowerData, points: ChartPoint[]) {
  const bySequence = new Map(target.chartPoints.map(point => [point.sequence, point]))
  for (const point of points)
    bySequence.set(point.sequence, point)

  target.chartPoints.splice(
    0,
    target.chartPoints.length,
    ...Array.from(bySequence.values())
      .sort((left, right) => left.sequence - right.sequence)
      .slice(-MAX_STATISTICS_LENGTH),
  )
  rebuildStatistics(target)
}

function replaceCurrentChart(target: RawPowerData, chart: CurrentChart) {
  if (target.chartSessionId !== chart.sessionId)
    retireChartSession(chart.deviceId, target.chartSessionId)
  target.chartPoints.splice(0, target.chartPoints.length, ...chart.points)
  target.chartSessionId = chart.sessionId
  rebuildStatistics(target)
}

function applyCurrentChartSnapshot(chart: CurrentChart, generationAtRequest: number) {
  const target = getChartData(chart.deviceId)
  const stateChangedWhileLoading
    = generationFor(chartStateGenerations, chart.deviceId) !== generationAtRequest

  // A reset or a point for a newer session won the race with this snapshot.
  // Never let the older command response switch the canonical store back.
  if (
    stateChangedWhileLoading
    && target.chartSessionId
    && target.chartSessionId !== chart.sessionId
  ) {
    return
  }

  if (
    target.chartSessionId !== chart.sessionId
    && isRetiredChartSession(chart.deviceId, chart.sessionId)
  ) {
    return
  }

  if (target.chartSessionId === chart.sessionId) {
    // Events can arrive between command invocation and response. Merge by
    // sequence so the snapshot fills gaps without discarding newer points.
    mergeChartPoints(target, chart.points)
  }
  else {
    replaceCurrentChart(target, chart)
  }
  incrementGeneration(chartStateGenerations, chart.deviceId)
}

function applyChartPoint(deviceId: string, sessionId: string, point: ChartPoint) {
  const target = getChartData(deviceId)
  if (target.chartSessionId !== sessionId) {
    if (isRetiredChartSession(deviceId, sessionId))
      return

    retireChartSession(deviceId, target.chartSessionId)
    target.chartPoints.splice(0)
    target.statistics.splice(0)
    target.chartSessionId = sessionId
    target.lastChartSequence = 0
    incrementGeneration(chartEventSessionGenerations, deviceId)
  }
  if (target.chartPoints.some(existing => existing.sequence === point.sequence))
    return

  mergeChartPoints(target, [point])
  incrementGeneration(chartStateGenerations, deviceId)
}

async function loadCurrentChart(deviceId: string) {
  const loadToken = ++nextChartLoadToken
  const generationAtRequest = generationFor(chartStateGenerations, deviceId)
  chartLoadTokens.set(deviceId, loadToken)
  try {
    const result = await commands.getCurrentChart(deviceId)
    if (chartLoadTokens.get(deviceId) !== loadToken)
      return

    if (result.status === 'ok')
      applyCurrentChartSnapshot(result.data, generationAtRequest)
    else
      console.error(result.error)
  }
  catch (error) {
    console.error(error)
  }
}

function applyChartReset(payload: { deviceId: string, sessionId: string }) {
  const target = getChartData(payload.deviceId)
  if (
    target.chartSessionId !== payload.sessionId
    && isRetiredChartSession(payload.deviceId, payload.sessionId)
  ) {
    return
  }

  if (target.chartSessionId !== payload.sessionId)
    retireChartSession(payload.deviceId, target.chartSessionId)
  target.chartPoints.splice(0)
  target.statistics.splice(0)
  target.chartSessionId = payload.sessionId
  target.lastChartSequence = 0
  incrementGeneration(chartEventSessionGenerations, payload.deviceId)
  incrementGeneration(chartStateGenerations, payload.deviceId)
}

void chartListenersReady
  .then(() => loadCurrentChart('local'))
  .catch(error => console.error(error))

events.deviceEvent.listen(({ payload }) => {
  const deviceData = getOrCreateDeviceData(payload.udid)

  if (payload.action === 'Attached') {
    deviceData.interface.add(payload.interface)
    deviceData.offline = false
  }
  else if (payload.action === 'Detached') {
    deviceData.interface.delete(payload.interface)
  }
  if (deviceData.interface.size === 0) {
    deviceData.offline = true
  }
})

const vis = useDocumentVisibility()
const tab = useTab()

const currentPower = computed<RawPowerData>(() => {
  return tab.value === 'local' ? power.local : power.remote[tab.value] || {}
})

export async function clearPowerChart(deviceId: string) {
  const eventSessionGenerationAtRequest
    = generationFor(chartEventSessionGenerations, deviceId)
  const result = await commands.clearCurrentChart(deviceId)
  if (result.status === 'error')
    return result

  const target = getChartData(result.data.deviceId)
  const aNewerSessionEventWon = generationFor(
    chartEventSessionGenerations,
    deviceId,
  ) > eventSessionGenerationAtRequest
  && target.chartSessionId !== ''
  && target.chartSessionId !== result.data.sessionId

  if (!aNewerSessionEventWon) {
    if (target.chartSessionId === result.data.sessionId) {
      // Usually the reset event arrives before the command response. Preserve
      // any first point of the new session that may already have followed it.
      mergeChartPoints(target, result.data.points)
    }
    else {
      replaceCurrentChart(target, result.data)
    }
    incrementGeneration(chartStateGenerations, result.data.deviceId)
  }

  return result
}

export function usePower() {
  return computed(() => ({
    ...currentPower.value.data,
    isLoading: Object.keys(currentPower.value.data).length === 0 || vis.value === 'hidden',
    isRemote: tab.value !== 'local',
    statistics: currentPower.value.statistics,
  }))
}

export function usePowerData() {
  return power
}

export function usePowerRaw() {
  return computed<
    RawPowerData & { isLocal: true } |
    RemotePowerData & { isLocal: false }
  >(() => {
    const isLocal = tab.value === 'local'
    return {
      ...isLocal ? currentPower.value : power.remote[tab.value],
      isLocal,
    } as any
  })
}
