<script setup lang="ts">
import type { ChargingHistory } from '@/bindings'
import { commands } from '@/bindings'
import { Button } from '@/components/ui/button'
import { useHistory } from '@/composables/useHistory'
import { confirm } from '@tauri-apps/plugin-dialog'
import { AlertCircle, Info, Loader2, RefreshCw, Trash2 } from 'lucide-vue-next'
import { useI18n } from 'vue-i18n'

const { selectedItem, detailRevision, history: { data, isLoading, err, update } } = useHistory()
const { t } = useI18n()
const deleting = ref(false)
const deleteError = ref('')
const cleanupRetryNeeded = ref(false)
const retryingCleanup = ref(false)

async function retryHistoryCleanup() {
  retryingCleanup.value = true
  try {
    const result = await commands.retryHistoryCleanup()
    if (result.status === 'error') {
      deleteError.value = `${t('history.cleanup_retry_failed')}: ${result.error}`
      return
    }

    cleanupRetryNeeded.value = false
    deleteError.value = ''
  }
  catch (error) {
    deleteError.value = `${t('history.cleanup_retry_failed')}: ${String(error)}`
  }
  finally {
    retryingCleanup.value = false
  }
}

async function deleteAllHistory() {
  deleteError.value = ''

  try {
    const confirmed = await confirm(t('history.delete_all_message'), {
      title: t('history.delete_all_title'),
      kind: 'warning',
      okLabel: t('history.delete_all_confirm'),
      cancelLabel: t('history.cancel'),
    })
    if (!confirmed)
      return

    deleting.value = true
    const result = await commands.deleteAllHistory()
    if (result.status === 'error') {
      selectedItem.value = null
      await update()
      deleteError.value = `${t('history.delete_failed')}: ${result.error}`
      return
    }

    selectedItem.value = null
    await update()
    if (!result.data.cleanupComplete) {
      cleanupRetryNeeded.value = true
      deleteError.value = `${t('history.cleanup_incomplete')}: ${result.data.cleanupError ?? t('history.unknown_error')}`
    }
    else {
      cleanupRetryNeeded.value = false
    }
  }
  catch (error) {
    deleteError.value = `${t('history.delete_failed')}: ${String(error)}`
  }
  finally {
    deleting.value = false
  }
}
</script>

<template>
  <div
    v-if="!isLoading && err"
    class="w-full h-full flex flex-col gap-3 items-center justify-center text-red-500"
  >
    <AlertCircle class="w-6 h-6" />
    <span>{{ $t('history.load_failed') }}</span>
    <span class="max-w-lg text-center text-xs">{{ err }}</span>
    <Button variant="outline" size="sm" @click="update">
      <RefreshCw class="size-4" />
      {{ $t('history.retry') }}
    </Button>
  </div>
  <div
    v-else-if="!isLoading && !data?.length"
    class="w-full h-full flex flex-col gap-2 items-center justify-center text-muted-foreground"
  >
    <Info class="w-6 h-6" />
    <span>{{ $t('history.empty') }}</span>
    <span class="mb-16 text-xs">{{ $t('history.empty_desc') }}</span>
    <span v-if="deleteError" class="max-w-lg text-center text-xs text-red-500">
      {{ deleteError }}
    </span>
    <Button
      v-if="cleanupRetryNeeded"
      variant="outline"
      size="sm"
      :disabled="retryingCleanup"
      @click="retryHistoryCleanup"
    >
      <Loader2 v-if="retryingCleanup" class="size-4 animate-spin" />
      <RefreshCw v-else class="size-4" />
      {{ retryingCleanup ? $t('history.retrying_cleanup') : $t('history.retry_cleanup') }}
    </Button>
  </div>
  <div v-else class="flex h-[calc(100vh-80px)]">
    <div class="flex flex-col gap-4 pl-4 ">
      <div class="flex items-center justify-between gap-3 pr-4">
        <h2 class="font-bold text-lg">
          {{ $t('history.title') }}
        </h2>
        <Button
          v-if="!isLoading && data?.length"
          variant="destructive"
          size="sm"
          :disabled="deleting"
          @click="deleteAllHistory"
        >
          <Loader2 v-if="deleting" class="size-4 animate-spin" />
          <Trash2 v-else class="size-4" />
          {{ $t('history.delete_all') }}
        </Button>
      </div>
      <p v-if="deleteError" class="max-w-64 pr-4 text-xs text-red-500">
        {{ deleteError }}
      </p>
      <Button
        v-if="cleanupRetryNeeded"
        variant="outline"
        size="sm"
        class="self-start"
        :disabled="retryingCleanup"
        @click="retryHistoryCleanup"
      >
        <Loader2 v-if="retryingCleanup" class="size-4 animate-spin" />
        <RefreshCw v-else class="size-4" />
        {{ retryingCleanup ? $t('history.retrying_cleanup') : $t('history.retry_cleanup') }}
      </Button>
      <div v-if="!isLoading && data" class="flex flex-col gap-4 h-full overflow-y-auto pr-4">
        <HistoryListItem
          v-for="item in data as ChargingHistory[]"
          :key="item.id"
          v-bind="item"
          class="cursor-pointer transition-colors"
          :class="{ 'bg-muted': selectedItem?.id === item.id }"
          @click="selectedItem = selectedItem?.id === item.id ? null : item"
        />
        <div class="my-4 font-mono" />
      </div>
    </div>
    <Separator orientation="vertical" class="h-auto" />
    <div class="relative grow">
      <HistoryDetail
        v-if="selectedItem?.id"
        :key="`${selectedItem.id}:${detailRevision}`"
        v-bind="selectedItem"
        class="h-full"
      />
      <div v-else class="overflow-y-auto h-full flex flex-col items-center justify-center">
        <Info class="size-6" />
        <p class="text-muted-foreground font-medium text-sm mb-10">
          {{ $t('history.select_session') }}
        </p>
      </div>
    </div>
  </div>
</template>
