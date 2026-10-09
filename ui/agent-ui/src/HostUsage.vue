<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RefreshCw } from "@lucide/vue";
import {
  Button,
  LoadingIndicator,
  Popover,
  PopoverContent,
  PopoverTrigger,
  Progress,
} from "@ivy/ui";
import { nativeRead, record, text } from "../../../packages/ui-client/src/native";
import { client } from "./runtime";

// Account usage and limits of one host, read from its native owner whenever the popover opens.
const props = defineProps<{ node: string; label: string; ready: boolean }>();
const open = ref(false), loading = ref(false), error = ref(""), result = ref<Record<string, unknown> | null>(null);
let controller: AbortController | null = null;
const load = async () => {
  controller?.abort();
  const current = controller = new AbortController();
  loading.value = true;
  error.value = "";
  try {
    const value = record(await nativeRead(client, props.node, "codex.account/rateLimits/read", null, current.signal));
    if (current === controller) result.value = value;
  } catch (cause) {
    if (current === controller) error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    if (current === controller) loading.value = false;
  }
};
watch(open, value => { if (value) void load(); else controller?.abort(); });

interface Window { label: string; used: number; resets: string }
interface Bucket { id: string; name: string; plan: string; windows: Window[]; notes: string[] }
const windowLabel = (minutes: number) =>
  !Number.isFinite(minutes) || minutes <= 0 ? "Usage"
    : minutes % 10080 === 0 ? (minutes === 10080 ? "Weekly limit" : minutes / 10080 + "-week limit")
    : minutes % 1440 === 0 ? (minutes === 1440 ? "Daily limit" : minutes / 1440 + "-day limit")
    : minutes % 60 === 0 ? minutes / 60 + "-hour limit" : minutes + "-minute limit";
const resetLabel = (seconds: unknown) => {
  const value = Number(seconds) * 1000;
  if (!Number.isFinite(value) || value <= 0) return "";
  const date = new Date(value), sameDay = date.toDateString() === new Date().toDateString();
  return "Resets " + (sameDay
    ? date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : date.toLocaleString([], { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }));
};
const percent = (value: unknown) => Math.min(100, Math.max(0, Math.round(Number(value) || 0)));
const buckets = computed<Bucket[]>(() => {
  const value = result.value;
  if (!value) return [];
  const byId = record(value.rateLimitsByLimitId);
  const entries = Object.keys(byId).length ? Object.entries(byId) : [["", value.rateLimits] as const];
  return entries.map(([id, raw]) => {
    const limit = record(raw), windows: Window[] = [], notes: string[] = [];
    for (const part of [limit.primary, limit.secondary]) {
      const window = record(part);
      if (typeof window.usedPercent === "number")
        windows.push({ label: windowLabel(Number(window.windowDurationMins)), used: percent(window.usedPercent), resets: resetLabel(window.resetsAt) });
    }
    const individual = record(limit.individualLimit);
    if (typeof individual.remainingPercent === "number")
      windows.push({ label: "Your limit · " + text(individual.used) + " of " + text(individual.limit),
        used: percent(100 - individual.remainingPercent), resets: resetLabel(individual.resetsAt) });
    const credits = record(limit.credits);
    if (credits.unlimited === true) notes.push("Unlimited credits");
    else if (credits.hasCredits === true && text(credits.balance)) notes.push("Credits: " + text(credits.balance));
    if (text(limit.rateLimitReachedType)) notes.push("Limit reached: " + text(limit.rateLimitReachedType).replaceAll("_", " "));
    if (limit.spendControlReached === true) notes.push("Spend control reached");
    return { id: id || text(limit.limitId), name: text(limit.limitName) || text(limit.limitId) || id || "Usage", plan: text(limit.planType), windows, notes };
  }).filter(bucket => bucket.windows.length || bucket.notes.length);
});
const resetCredits = computed(() => Number(record(result.value?.rateLimitResetCredits).availableCount) || 0);
const blocked = computed(() => result.value?.ordinaryUsageAllowed === false);
</script>
<template>
  <Popover v-model:open="open">
    <PopoverTrigger as-child><slot /></PopoverTrigger>
    <PopoverContent align="start" class="w-80 space-y-4 p-4">
      <div class="flex items-center gap-2">
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-medium">{{ label }}</p>
          <p class="text-xs text-muted-foreground">{{ ready ? "Usage and limits" : "Usage and limits · host not ready" }}</p>
        </div>
        <Button variant="ghost" size="icon-sm" aria-label="Refresh usage" :disabled="loading" @click="load"
          ><RefreshCw :class="{ 'animate-spin': loading }" aria-hidden="true"
        /></Button>
      </div>
      <LoadingIndicator v-if="loading && !result" label="Loading usage…" />
      <p v-else-if="error" role="alert" class="text-sm text-destructive">{{ error }}</p>
      <template v-else-if="result">
        <p v-if="blocked" role="alert" class="text-sm text-destructive">Included usage is currently not available on this account.</p>
        <section v-for="bucket in buckets" :key="bucket.id || bucket.name" class="space-y-3" :aria-label="bucket.name">
          <p v-if="buckets.length > 1 || bucket.plan" class="flex items-center gap-2 text-xs font-medium text-muted-foreground">
            <span class="truncate">{{ bucket.name }}</span><span v-if="bucket.plan" class="ml-auto rounded bg-muted px-1.5 py-0.5 uppercase">{{ bucket.plan }}</span>
          </p>
          <div v-for="window in bucket.windows" :key="window.label" class="space-y-1.5">
            <div class="flex items-baseline gap-2 text-sm">
              <span class="min-w-0 flex-1 truncate">{{ window.label }}</span>
              <span class="tabular-nums text-muted-foreground">{{ window.used }}% used</span>
            </div>
            <Progress :model-value="window.used" :aria-label="window.label" />
            <p v-if="window.resets" class="text-xs text-muted-foreground">{{ window.resets }}</p>
          </div>
          <p v-for="note in bucket.notes" :key="note" class="text-xs text-muted-foreground">{{ note }}</p>
        </section>
        <p v-if="resetCredits" class="text-xs text-muted-foreground">{{ resetCredits }} limit reset {{ resetCredits === 1 ? "credit" : "credits" }} available</p>
        <p v-if="!buckets.length && !blocked" class="text-sm text-muted-foreground">This host reports no usage limits.</p>
      </template>
    </PopoverContent>
  </Popover>
</template>
