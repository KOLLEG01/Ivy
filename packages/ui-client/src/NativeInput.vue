<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { Disclosure, Button, ContentView, Input, Label, OptionSelect, StatusBadge, useRemote } from '@ivy/ui';
import type { Agent, RpcClient, Wire } from '../../sdk/src/client.js';
import { enumStrings, list, nativeRead, optionalTool, record, schemaNode, text } from './native';
import { useNativeAction } from './native-action';
import NativeActionState from './NativeActionState.vue';
const props = defineProps<{ compact?: boolean; input: Agent.PendingInput; epoch: string | null; available: boolean; base: URL; client: RpcClient }>();
const { base, client } = props;
const emit = defineEmits<{ answered: [] }>();
const identity = JSON.stringify(props.input.identity), key = 'ivy:agent-input:' + base.href + identity;
const action = useNativeAction(client, key);
watch(() => props.input.state, value => { if (value === 'answered' && action.saved.value?.phase === 'pending') void action.reconcile(); }, { immediate: true });
const definition = useRemote(async signal => props.input.state === 'pending' && props.epoch === props.input.identity.epoch ? await nativeRead(client, props.input.identity.serviceNodeId, 'agent.inputDefinition', { identity: props.input.identity }, signal) as Agent.InputDefinition : null);
const binding = useRemote(() => optionalTool(client, props.input.identity.serviceNodeId, 'agent.answer'));
const params = computed(() => record(props.input.params)), fields = computed(() => record(schemaNode(definition.value.value?.responseSchema, definition.value.value?.responseSchema).properties));
const questionMode = computed(() => props.input.method === 'item/tool/requestUserInput');
const questions = computed(() => list(params.value.questions).map(record).slice(0, 32));
const secret = computed(() => questions.value.some(q => q.isSecret === true));
const answers = ref<Record<string, string>>({}), decision = ref('');
try { const draft = JSON.parse(sessionStorage.getItem(key + ':draft') ?? '{}'); if (!secret.value) { answers.value = record(draft.answers) as Record<string, string>; decision.value = text(draft.decision); } } catch { action.error.value = 'The response draft could not be read.'; }
watch([answers, decision], () => { if (secret.value || props.input.state === 'answered' || action.saved.value?.phase === 'succeeded') return; try { sessionStorage.setItem(key + ':draft', JSON.stringify({ answers: answers.value, decision: decision.value })); } catch { action.error.value = 'The response draft cannot be retained.'; } }, { deep: true });
// A delivered answer needs no draft; the request then leaves the task view.
watch(() => action.saved.value?.phase, phase => { if (phase === 'succeeded') { try { sessionStorage.removeItem(key + ':draft'); } catch { /* Optional view state. */ } emit('answered'); } }, { immediate: true });
const decisions = computed(() => {
  if (!['item/commandExecution/requestApproval', 'item/fileChange/requestApproval'].includes(props.input.method)) return [];
  const values = enumStrings(fields.value.decision, definition.value.value?.responseSchema);
  return Array.isArray(params.value.availableDecisions) ? values.filter(v => list(params.value.availableDecisions).includes(v)) : values;
});
const permissionsMode = computed(() => props.input.method === 'item/permissions/requestApproval' && !!fields.value.permissions && enumStrings(fields.value.scope, definition.value.value?.responseSchema).includes('turn'));
const pendingNow = computed(() => props.available && props.input.state === 'pending' && props.input.identity.epoch === props.epoch && props.input.identity.epoch === definition.value.value?.identity.epoch && !!binding.value.value && !definition.error.value && !binding.error.value && (!action.locked.value || secret.value && action.saved.value?.phase === 'not_found' && !action.busy.value));
const canAnswer = computed(() => pendingNow.value && (questionMode.value ? !!fields.value.answers && questions.value.length > 0 && list(params.value.questions).length <= 32 && questions.value.every(q => typeof q.id === 'string' && answers.value[q.id]?.trim()) : decisions.value.includes(decision.value)));
const send = async (reply: Wire.Json) => {
  if (!pendingNow.value || !binding.value.value) return;
  const latest = await nativeRead(client, props.input.identity.serviceNodeId, 'agent.inputs', { identity: props.input.identity }) as Agent.PendingInputPage;
  if (latest.epoch !== props.input.identity.epoch || latest.items[0]?.state !== 'pending') { action.error.value = 'This native request has been answered or expired. The response draft is retained.'; emit('answered'); return; }
  if (secret.value && action.saved.value?.phase === 'not_found') await action.reenterSecret({ identity: props.input.identity, reply: { result: reply } });
  else await action.start('Answer native request', binding.value.value, { identity: props.input.identity, reply: { result: reply } }, secret.value);
  emit('answered');
};
const submit = async () => {
  if (!canAnswer.value) return;
  const reply = questionMode.value ? { answers: Object.fromEntries(questions.value.map(q => [text(q.id), { answers: [answers.value[text(q.id)]!] }])) } : { decision: decision.value };
  try { await send(reply); } catch (cause) { action.error.value = cause instanceof Error ? cause.message : 'The current owner could not be checked.'; }
};
const permission = async (allow: boolean) => { try { await send({ permissions: allow ? record(params.value.permissions) : {}, scope: 'turn' }); } catch (cause) { action.error.value = String(cause); } };
const decisionName = (value: string) => ({ accept: 'Approve once', acceptForSession: 'Approve for this session', decline: 'Decline', cancel: 'Cancel request' }[value] ?? value);
</script>
<template><article class="space-y-4 rounded-xl border bg-card p-5"><div class="flex flex-wrap items-center gap-3"><h3 class="font-semibold">{{ input.method === 'item/tool/requestUserInput' ? (compact ? 'Question' : 'Native question') : input.method.includes('Approval') ? (compact ? 'Approval' : 'Native approval') : input.method }}</h3><StatusBadge :label="input.state" :tone="input.state === 'pending' ? 'warning' : 'neutral'" /></div>
  <p v-if="!compact" class="break-all text-xs text-muted-foreground">{{ input.identity.serviceNodeId }} · connection {{ input.identity.epoch }} · request {{ JSON.stringify(input.identity.requestId) }} · {{ input.observedAt }}</p>
  <p v-if="input.state !== 'pending' || input.identity.epoch !== epoch" class="text-sm text-muted-foreground">This retained request is not answerable on the current native connection.</p>
  <p v-if="definition.error.value || binding.error.value" class="text-sm text-destructive" role="alert">{{ definition.error.value ?? binding.error.value }}</p>
  <p v-if="params.reason" class="text-sm">{{ params.reason }}</p><p v-if="params.cwd" class="break-all text-sm text-muted-foreground">Working directory: {{ params.cwd }}</p><ContentView v-if="typeof params.command === 'string'" :text="params.command" media-type="text/plain" />
  <template v-if="questionMode"><div v-for="question in questions" :key="text(question.id)" class="space-y-3"><Label :for="identity + text(question.id)">{{ question.question }}</Label><div v-if="list(question.options).length" class="flex flex-wrap gap-2"><Button v-for="option in list(question.options)" :key="text(record(option).label)" variant="outline" size="sm" :disabled="!pendingNow" @click="answers[text(question.id)] = text(record(option).label)">{{ record(option).label }}</Button></div><Input :id="identity + text(question.id)" :model-value="answers[text(question.id)] ?? ''" @update:model-value="answers[text(question.id)] = String($event)" :type="question.isSecret ? 'password' : 'text'" :disabled="!pendingNow" autocomplete="off" :maxlength="65536" /></div><p v-if="secret" class="text-xs text-muted-foreground">Secret answers remain in memory and are not retained after reload.</p></template>
  <div v-else-if="decisions.length" class="space-y-2"><Label :for="identity + ':decision'">Decision</Label><OptionSelect :id="identity + ':decision'" v-model="decision" :disabled="!pendingNow" class="w-full"><option value="">Choose a decision</option><option v-for="value in decisions" :key="value" :value="value">{{ decisionName(value) }}</option></OptionSelect></div>
  <div v-else-if="permissionsMode" class="space-y-3"><p class="text-sm">Requested permissions, limited to this turn:</p><ContentView :text="JSON.stringify(params.permissions, null, 2)" /><div class="flex flex-wrap gap-2"><Button :disabled="!pendingNow" @click="permission(true)">Allow requested permissions for this turn</Button><Button variant="outline" :disabled="!pendingNow" @click="permission(false)">Decline permissions</Button></div></div>
  <p v-else-if="definition.value.value" class="text-sm text-muted-foreground">This native response shape has no supported form in this UI release. Use a compatible native client; no guessed response will be sent.</p>
  <Button v-if="questionMode || decisions.length" :disabled="!canAnswer" :loading="action.pending.value" @click="submit">Send response</Button>
  <Disclosure title="Full native request"><ContentView :text="JSON.stringify(input.params, null, 2)" /></Disclosure>
  <NativeActionState :action="action.saved.value" :busy="action.busy.value" :error="action.error.value" @reconcile="action.reconcile" @retry="action.retry" />
</article></template>
