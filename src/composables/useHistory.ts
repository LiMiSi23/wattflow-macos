import { type ChargingHistory, commands, events, type Result } from '@/bindings'

export function useAsyncData<T>(promiseFn: () => Promise<Result<T, string>>) {
  const data = ref<T | null>(null)
  const isLoading = ref(true)
  const err = ref('')
  let requestId = 0

  const load = async () => {
    const currentRequestId = ++requestId
    try {
      const r = await promiseFn()
      if (currentRequestId !== requestId)
        return

      if (r.status === 'ok') {
        data.value = r.data
      }
      else {
        err.value = r.error
        console.error(r.error)
      }
    }
    catch (error) {
      if (currentRequestId !== requestId)
        return

      err.value = error instanceof Error ? error.message : String(error)
      console.error(error)
    }
    finally {
      if (currentRequestId === requestId)
        isLoading.value = false
    }
  }

  const update = () => {
    isLoading.value = true
    data.value = null
    err.value = ''
    return load()
  }

  load()

  return {
    data,
    isLoading,
    err,
    update,
  }
}

const selectedItem = ref(null as ChargingHistory | null)
const history = useAsyncData<ChargingHistory[]>(() => commands.getAllChargingHistory())
const detailRevision = ref(0)
let recordedRefreshToken = 0

void events.historyRecordedEvent.listen(() => {
  const refreshToken = ++recordedRefreshToken
  const selectedId = selectedItem.value?.id

  void history.update().then(() => {
    if (refreshToken !== recordedRefreshToken)
      return

    // Keep the existing selection/detail visible if refreshing the list failed.
    // The page already exposes the load error and offers a retry.
    if (!history.data.value)
      return

    if (selectedId !== undefined) {
      selectedItem.value
        = history.data.value.find(item => item.id === selectedId) ?? null
    }
    // A session is upserted under the same id. The explicit revision remounts
    // its detail view so the curve is fetched again even though the id did not
    // change.
    detailRevision.value++
  })
}).catch(error => console.error(error))

export function useHistory() {
  return {
    selectedItem,
    history,
    detailRevision,
  }
}
