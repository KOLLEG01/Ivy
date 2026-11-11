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
  UsagePill,
  useRemote,
} from "@ivy/ui";
import type { UsageMeter, UsagePillLevel } from "@ivy/ui";
import type { Agent, HiveClient } from "../../sdk/src/client.js";
import { nativeRead, optionalTool, record } from "./native";
import { backendRead, nativeBackendLabel, nativeBackendKey, nativeMethod, sameBackend } from './native-backends';
import type { NativeBackendScope } from './native-backends';
import { accountGroups, metricsStale, sharedRead, systemMeters, usageBuckets } from './host-load';
import type { AccountUsage } from './host-load';

// Host load and every account's limits, read at most about once a minute and shared by visible tabs.
const props = defineProps<{ client: HiveClient; node: string; backend?: Agent.BackendIdentity | undefined; accounts?: NativeBackendScope[] | undefined; label: string; ready: boolean; level?: UsagePillLevel; passive?: boolean }>();
const open = ref(false);
type Owner = { serviceNodeId: string; backend?: Agent.BackendIdentity | undefined; status: { state: string } };
const owners = computed<Owner[]>(() => props.accounts
  ? props.accounts.filter(value => !props.backend || value.serviceNodeId === props.node && sameBackend(value.backend, props.backend))
  : [{ serviceNodeId: props.node, backend: props.backend, status: { state: props.ready ? 'ready' : 'offline' } }]);
const ownerKey = (owner: { serviceNodeId: string; backend?: Agent.BackendIdentity | undefined }) => JSON.stringify([owner.serviceNodeId, nativeBackendKey(owner.backend)]);
const sharedKey = (...parts: string[]) => JSON.stringify([location.pathname.split('/ui/')[0], ...parts]);
let force = false;
const active = () => props.ready && (!props.passive || open.value);

const metrics = useRemote(async signal => {
  if (!active()) return null;
  return sharedRead(sharedKey(props.node, 'agent.hostMetrics'), 55000, async () =>
    await optionalTool(props.client, props.node, 'agent.hostMetrics')
      ? await nativeRead(props.client, props.node, 'agent.hostMetrics', {}, signal) as Agent.HostMetrics : null, force);
}, 60000);
const usage = useRemote(async signal => {
  if (!active()) return [];
  const reads = await Promise.all(owners.value.map(async (owner): Promise<AccountUsage> => {
    const label = owner.backend ? nativeBackendLabel(owner.backend) : props.label;
    const ready = owner.status.state === 'ready';
    if (!ready) return { key: ownerKey(owner), label, ready, result: null, error: null };
    try {
      const result = await sharedRead(sharedKey(ownerKey(owner), 'account/rateLimits/read'), 55000, async () =>
        await optionalTool(props.client, owner.serviceNodeId, nativeMethod(owner.backend, 'account/rateLimits/read'))
          ? record(await backendRead(props.client, owner, 'account/rateLimits/read', null, signal)) : null, force);
      return { key: ownerKey(owner), label, ready, result, error: null };
    } catch (cause) {
      signal.throwIfAborted();
      return { key: ownerKey(owner), label, ready, result: null, error: cause instanceof Error ? cause.message : 'Usage unavailable.' };
    }
  }));
  return reads;
}, 60000);
const loading = computed(() => metrics.loading.value || usage.loading.value);
const refresh = async () => { force = true; try { await Promise.all([metrics.refresh(), usage.refresh()]); } finally { force = false; } };
watch(open, value => { if (value && props.passive) void refresh(); });
watch(() => JSON.stringify([props.node, props.ready, owners.value.map(ownerKey)]), () => {
  metrics.value.value = null; usage.value.value = null; void metrics.refresh(); void usage.refresh();
});

const host = computed(() => metrics.value.value);
const stale = computed(() => !!host.value && metricsStale(host.value));
const measured = computed(() => host.value ? new Date(host.value.observedAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '');
const system = computed<UsageMeter[]>(() => host.value
  ? systemMeters(host.value).map(meter => stale.value ? { ...meter, used: null, detail: 'last measured ' + measured.value } : meter)
  : []);
const accounts = computed(() => usage.value.value ?? owners.value.map(owner => ({ key: ownerKey(owner), label: owner.backend ? nativeBackendLabel(owner.backend) : props.label, ready: owner.status.state === 'ready', result: null, error: null })));
const groups = computed(() => [...(system.value.length ? [{ id: 'host', meters: system.value }] : []), ...accountGroups(accounts.value)]);
const status = computed(() => metrics.error.value ? 'host load unavailable' : stale.value ? 'stale since ' + measured.value : '');
</script>
<template>
  <Popover v-model:open="open">
    <PopoverTrigger as-child><slot><UsagePill :label="label" :seed="node" :groups="groups" :level="level" :status="status" :unavailable="!ready" /></slot></PopoverTrigger>
    <PopoverContent align="start" class="max-h-[min(36rem,80dvh,var(--reka-popover-content-available-height))] w-80 space-y-4 overflow-y-auto p-4">
      <div class="flex items-center gap-2">
        <div class="min-w-0 flex-1">
          <p class="truncate text-sm font-medium">{{ label }}</p>
          <p class="text-xs text-muted-foreground">{{ !ready ? "Host offline" : host ? (stale ? "Stale · last measured " : "Measured ") + measured : "Load and limits" }}</p>
        </div>
        <Button variant="ghost" size="icon-sm" aria-label="Refresh usage" :disabled="loading" @click="refresh"
          ><RefreshCw :class="{ 'animate-spin': loading }" aria-hidden="true"
        /></Button>
      </div>
      <LoadingIndicator v-if="loading && !host && !usage.value.value" label="Loading usage…" />
      <section v-if="system.length" class="space-y-3" aria-label="Host load">
        <div v-for="meter in system" :key="meter.id" class="space-y-1.5">
          <div class="flex items-baseline gap-2 text-sm">
            <span class="min-w-0 flex-1 truncate">{{ meter.label }}</span>
            <span class="tabular-nums text-muted-foreground">{{ meter.used === null ? "—" : meter.used + "% used" }}</span>
          </div>
          <Progress v-if="meter.used !== null" :model-value="meter.used" :aria-label="meter.label" />
          <p v-if="meter.detail" class="text-xs break-words text-muted-foreground">{{ meter.detail }}</p>
        </div>
      </section>
      <p v-else-if="metrics.error.value" role="alert" class="text-sm text-destructive">{{ metrics.error.value }}</p>
      <section v-for="account in accounts" :key="account.key" class="space-y-3 border-t pt-3" :aria-label="account.label">
        <p class="truncate text-xs font-medium text-muted-foreground">{{ account.label }}</p>
        <p v-if="!account.ready" class="text-sm text-muted-foreground">Account not ready.</p>
        <p v-else-if="account.error" role="alert" class="text-sm text-destructive">{{ account.error }}</p>
        <template v-else-if="account.result">
          <p v-if="account.result.ordinaryUsageAllowed === false" role="alert" class="text-sm text-destructive">Included usage is currently not available on this account.</p>
          <div v-for="bucket in usageBuckets(account.result)" :key="bucket.id || bucket.name" class="space-y-3">
            <p v-if="usageBuckets(account.result).length > 1 || bucket.plan" class="flex items-center gap-2 text-xs text-muted-foreground">
              <span class="truncate">{{ bucket.name }}</span><span v-if="bucket.plan" class="ml-auto rounded bg-muted px-1.5 py-0.5 uppercase">{{ bucket.plan }}</span>
            </p>
            <div v-for="window in bucket.windows" :key="window.label" class="space-y-1.5">
              <div class="flex items-baseline gap-2 text-sm">
                <span class="min-w-0 flex-1 truncate">{{ window.label }}</span>
                <span class="tabular-nums text-muted-foreground">{{ window.used }}% used</span>
              </div>
              <Progress :model-value="window.used" :aria-label="account.label + ' · ' + window.label" />
              <p v-if="window.resets" class="text-xs text-muted-foreground">{{ window.resets }}</p>
            </div>
            <p v-for="note in bucket.notes" :key="note" class="text-xs text-muted-foreground">{{ note }}</p>
          </div>
          <p v-if="Number(record(account.result.rateLimitResetCredits).availableCount) > 0" class="text-xs text-muted-foreground">
            {{ Number(record(account.result.rateLimitResetCredits).availableCount) }} limit reset credit(s) available</p>
          <p v-if="!usageBuckets(account.result).length && account.result.ordinaryUsageAllowed !== false" class="text-sm text-muted-foreground">
            {{ Object.keys(record(account.result.rateLimitsByLimitId)).length || record(account.result.rateLimits).limitId ? "This account has not reported usage yet." : "This account reports no usage limits." }}</p>
        </template>
        <p v-else-if="usage.value.value" class="text-sm text-muted-foreground">This account does not report usage limits.</p>
      </section>
    </PopoverContent>
  </Popover>
</template>
