<script setup lang="ts">
import type { ChartHistoryErrorEvent } from '@/bindings'
import { events } from '@/bindings'
import { AlertTriangle, X } from 'lucide-vue-next'

const error = ref<ChartHistoryErrorEvent | null>(null)

const unlisten = events.chartHistoryErrorEvent.listen(({ payload }) => {
  error.value = payload
})

onScopeDispose(() => {
  void unlisten.then(stop => stop()).catch(error => console.error(error))
})
</script>

<template>
  <Transition
    enter-active-class="transition duration-200"
    enter-from-class="-translate-y-2 opacity-0"
    leave-active-class="transition duration-150"
    leave-to-class="-translate-y-2 opacity-0"
  >
    <div
      v-if="error"
      role="alert"
      class="fixed left-1/2 top-3 z-[100] flex max-w-[min(42rem,calc(100vw-2rem))] -translate-x-1/2 items-start gap-3 rounded-lg border border-red-500/40 bg-background px-4 py-3 text-sm shadow-lg"
    >
      <AlertTriangle class="mt-0.5 size-4 shrink-0 text-red-500" />
      <div class="min-w-0 grow">
        <p class="font-semibold text-red-600 dark:text-red-400">
          {{ $t('chart.history_error') }}
        </p>
        <p class="break-words text-xs text-muted-foreground">
          {{ error.operation }}: {{ error.message }}
        </p>
      </div>
      <button
        type="button"
        class="shrink-0 rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        :aria-label="$t('chart.dismiss_error')"
        @click="error = null"
      >
        <X class="size-4" />
      </button>
    </div>
  </Transition>
</template>
