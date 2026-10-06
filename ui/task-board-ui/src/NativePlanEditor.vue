<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { Disclosure, Button, Label, OptionSelect, Textarea, RemoteState, useRemote } from '@ivy/ui';
import { discover } from '../../../packages/sdk/src/client.js';
import type { Agent } from '../../../packages/sdk/src/client.js';
import { enumStrings, list, modelsFrom, nativeModels, nativeRead, record, schemaNode, text } from '../../../packages/ui-client/src/native';
import { base, client } from './runtime';
import type { TaskBoard, Wire } from './runtime';
import { useAction } from './action';
import ActionState from './ActionState.vue';
import { hashJson } from '../../../packages/ui-client/src/hash';
import ProjectLocationPicker from '../../../packages/ui-client/src/ProjectLocationPicker.vue';
import { serviceTools } from '../../../packages/sdk/src/client.js';
const props = defineProps<{ workspace: TaskBoard.WorkspaceInfo; available: boolean; scope: string; prompt?: string; fixedNode?: string | undefined }>();
const emit = defineEmits<{ saved: [value: { pin: TaskBoard.ObjectPin; target: TaskBoard.Target; project: Wire.ResourceRef | null; plan: TaskBoard.NativePlanDraft }] }>();
const node = ref(props.fixedNode ?? ''), selectedProject = ref(''), model = ref(''), effort = ref(''), prompt = ref(props.prompt ?? '');
const action = useAction(() => props.workspace, 'plan:' + props.scope);
type PlanContext = { target: TaskBoard.Target; project: Wire.ResourceRef | null; plan: TaskBoard.NativePlanDraft };
const original = ref<PlanContext | null>(null), draftKey = 'ivy:task-board:plan-draft:' + base.href + props.workspace.serviceNodeId + ':' + props.workspace.callerPrincipalId + ':' + props.scope;
try { const saved = JSON.parse(sessionStorage.getItem(draftKey) ?? 'null'); if (saved && saved.principalId === props.workspace.principalId && saved.rootObjectId === props.workspace.rootObjectId) {
  node.value = props.fixedNode ?? saved.node; selectedProject.value = saved.selectedProject; model.value = saved.model; effort.value = saved.effort; prompt.value = props.scope.startsWith('continue:') ? props.prompt ?? saved.prompt : saved.prompt; original.value = saved.original;
} } catch { action.error.value = 'The native plan draft could not be read.'; }
const persistDraft = () => { try { sessionStorage.setItem(draftKey, JSON.stringify({ node: node.value, selectedProject: selectedProject.value, model: model.value, effort: effort.value, prompt: prompt.value, original: original.value, principalId: props.workspace.principalId, rootObjectId: props.workspace.rootObjectId })); } catch { throw new Error('The original native plan could not be retained before sending.'); } };
watch([node, selectedProject, model, effort, prompt], () => { try { persistDraft(); } catch (cause) { action.error.value = String(cause); } });
const recoveredPlan = () => { const pin = action.saved.value?.outcome?.plan; if (pin && original.value && action.identityMatches.value) emit('saved', { ...original.value, pin }); };
const hosts = useRemote(signal => client.request('serviceNodes.list', { serviceName: 'agent-manager', limit: 200 }, { signal }), 0, ["services"]);
const capabilities = useRemote(async signal => {
  if (!node.value) return null;
  const [status, projects, models, catalog, ...bindings] = await Promise.all([
    nativeRead(client, node.value, 'agent.status', {}, signal) as Promise<Agent.Status>,
    nativeRead(client, node.value, 'agent.projects', {}, signal) as Promise<Agent.ProjectsResult>, nativeModels(client, node.value, signal),
    nativeRead(client, node.value, 'agent.catalog', {}, signal) as Promise<Agent.Catalog>,
    ...['thread/start', 'thread/resume', 'turn/start', 'turn/interrupt'].map(method => discover(client, 'codex.' + method, { serviceNodeId: node.value })),
  ]);
  const current = await nativeRead(client, node.value, 'agent.status', {}, signal) as Agent.Status;
  if (status.state !== 'ready' || current.state !== 'ready' || !status.epoch || current.epoch !== status.epoch ||
    current.serviceNodeId !== node.value || status.serviceNodeId !== node.value || current.hostId !== status.hostId ||
    current.nativeVersion !== status.nativeVersion || current.nativeExecutableHash !== status.nativeExecutableHash || current.catalogHash !== status.catalogHash ||
    catalog.version !== status.nativeVersion || catalog.nativeExecutableHash !== status.nativeExecutableHash || await hashJson(catalog) !== status.catalogHash)
    throw new Error('The native owner changed during discovery. Reload its capabilities.');
  for (const binding of bindings) if (binding.definition.nativeSchemaIdentity !== status.catalogHash || binding.definition.interfaceVersion !== status.nativeVersion ||
    await hashJson(binding.definition) !== binding.definitionHash) throw new Error('The native definition changed during discovery.');
  return { status, projects, models, catalog, schema: bindings[2]!.definition.inputSchema, bindings };
});
watch(node, () => { selectedProject.value = ''; model.value = ''; effort.value = ''; void capabilities.refresh(); });
watch(() => props.prompt, value => { if (value !== undefined && props.scope.startsWith('continue:')) prompt.value = value; });
const choices = computed(() => modelsFrom(capabilities.value.value?.models));
const efforts = computed(() => choices.value.find(item => item.model === model.value)?.efforts ?? []);
watch(model, () => { if (!efforts.value.includes(effort.value)) effort.value = ''; });
const projects = computed(() => capabilities.value.value?.projects.projects.flatMap(project => project.paths.map(path => ({ key: JSON.stringify([project.nativeId, path]), id: project.nativeId, path, name: project.name, kind: project.kind ?? 'existing' }))) ?? []);
const selectingLocation = ref(false);
const chooseLocation = async (location: Agent.ProjectLocation) => {
  const owner = node.value, previous = capabilities.value.value;
  selectingLocation.value = true;
  selectedProject.value = '';
  try {
    if (!previous) throw new Error('Load the native owner before selecting a project.');
    // Allocation changes the project inventory, not the installed native schemas or models.
    const observed = await nativeRead(client, owner, 'agent.projects', {}) as Agent.ProjectsResult;
    if (node.value !== owner || capabilities.value.value !== previous)
      throw new Error('The native owner changed while selecting the project. Reload its capabilities.');
    if (!observed.projects.some(project => project.nativeId === location.project.nativeId && project.paths.includes(location.cwd)))
      throw new Error('The allocated project is not in the current owner inventory. Refresh before selecting it.');
    capabilities.value.value = { ...previous, projects: observed };
    selectedProject.value = JSON.stringify([location.project.nativeId, location.cwd]);
  } catch (cause) { action.error.value = cause instanceof Error ? cause.message : String(cause); }
  finally { selectingLocation.value = false; }
};
const draft = () => {
  const data = capabilities.value.value; if (!data) throw new Error('Load this native owner first.');
  const path = projects.value.find(project => project.key === selectedProject.value); if (!path) throw new Error('Choose an actual project on this owner.');
  const input = data.schema, turn = schemaNode(input, input);
  const inputArray = schemaNode(record(turn.properties).input, input), itemSchema = schemaNode(inputArray.items, input), variants = list(itemSchema.oneOf ?? itemSchema.anyOf);
  const textShape = variants.map(value => schemaNode(value, input)).find(value => enumStrings(record(value.properties).type, input).includes('text'));
  const textInput = { type: 'text', text: prompt.value, ...(textShape && 'text_elements' in record(textShape.properties) ? { text_elements: [] } : {}) };
  const plan = { nativeVersion: data.status.nativeVersion, catalogSourceHash: data.catalog.sourceHash,
    location: { serviceNodeId: node.value, hostId: data.status.hostId, kind: path.kind === 'task' ? 'internal' : path.kind, cwd: path.path, projectId: path.id },
    definitions: { threadStart: data.bindings[0]!.definitionHash, threadResume: data.bindings[1]!.definitionHash, turnStart: data.bindings[2]!.definitionHash, turnInterrupt: data.bindings[3]!.definitionHash },
    threadStart: { cwd: path.path, ...(model.value ? { model: model.value } : {}) }, threadResume: { cwd: path.path, excludeTurns: true },
    turnStart: { input: [textInput], ...(model.value ? { model: model.value } : {}), ...(effort.value ? { effort: effort.value } : {}) } } satisfies TaskBoard.NativePlanDraft;
  return { plan, target: { serviceNodeId: node.value, hostId: data.status.hostId }, project: { namespace: 'codex', kind: 'project', serviceNodeId: node.value, nativeId: path.id } };
};
const save = async () => {
  try { const value = draft();
    const verified = await serviceTools(client, node.value, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).call('agent.resolveProject', { selection: { kind: 'existing', cwd: value.plan.location.cwd }, expectedProjectId: value.plan.location.projectId }) as Agent.ProjectLocation;
    if (verified.project.nativeId !== value.plan.location.projectId) throw new Error('The selected project identity changed during verification.');
    const canonicalValue: PlanContext = { ...value, plan: { ...value.plan,
      location: { ...value.plan.location, kind: verified.kind === 'task' ? 'internal' : verified.kind, cwd: verified.cwd, projectId: verified.project.nativeId },
      threadStart: { ...value.plan.threadStart, cwd: verified.cwd }, threadResume: { ...value.plan.threadResume, cwd: verified.cwd } } };
    original.value = canonicalValue; persistDraft(); const outcome = await action.start('Save execution plan', { action: 'savePlan', plan: canonicalValue.plan }); if (outcome?.plan) emit('saved', { ...canonicalValue, pin: outcome.plan }); }
  catch (cause) { action.error.value = cause instanceof Error ? cause.message : 'The execution plan could not be prepared.'; }
};
const canSave = computed(() => props.available && props.workspace.role === 'user' && !action.locked.value && !capabilities.error.value && capabilities.value.value?.status.state === 'ready' && projects.value.some(value => value.key === selectedProject.value) && prompt.value.trim() && (!model.value || choices.value.some(value => value.model === model.value)) && (!effort.value || efforts.value.includes(effort.value)));
const inspectPlan = computed(() => { try { return JSON.stringify(draft().plan, null, 2); } catch { return null; } });
</script>
<template><section class="space-y-4 rounded-lg border bg-muted/20 p-4"><h3 class="font-semibold">Native execution plan</h3><p class="text-sm text-muted-foreground">Choose the owner, project and exact input for this work. Native configuration supplies permissions.</p>
  <div class="grid gap-4 md:grid-cols-2"><div class="space-y-2"><Label for="plan-owner">Native owner</Label><OptionSelect id="plan-owner" v-model="node" :disabled="!!fixedNode || action.locked.value"><option value="">Choose a native owner</option><option v-if="fixedNode && !hosts.value.value?.items.some(value => value.serviceNodeId === fixedNode)" :value="fixedNode">{{ fixedNode }}</option><option v-for="host in hosts.value.value?.items" :key="host.serviceNodeId" :value="host.serviceNodeId">{{ host.hostId }} · {{ host.serviceNodeId }}{{ host.ready ? '' : ' · unavailable' }}</option></OptionSelect></div>
    <div class="space-y-2"><Label for="plan-project">Native project</Label><OptionSelect id="plan-project" v-model="selectedProject" :disabled="action.locked.value"><option value="">Choose a project</option><option v-for="project in projects" :key="project.key" :value="project.key">{{ project.name }} · {{ project.path }}</option></OptionSelect></div>
    <div class="space-y-2"><Label for="plan-model">Model</Label><OptionSelect id="plan-model" v-model="model" :disabled="action.locked.value"><option value="">Native configured default</option><option v-for="choice in choices" :key="choice.id" :value="choice.model">{{ choice.name }}</option></OptionSelect></div>
    <div class="space-y-2"><Label for="plan-effort">Reasoning effort</Label><OptionSelect id="plan-effort" v-model="effort" :disabled="!model || action.locked.value"><option value="">Native configured default</option><option v-for="value in efforts" :key="value">{{ value }}</option></OptionSelect></div></div>
  <div class="space-y-2"><Label for="plan-prompt">Native task input</Label><Textarea id="plan-prompt" v-model="prompt" :disabled="action.locked.value || props.prompt !== undefined && scope.startsWith('continue:')" :maxlength="1048576" class="min-h-36" /></div>
  <ProjectLocationPicker v-if="node" :key="node" :client="client" :node="node" :scope="'task-board:' + base.href + ':' + scope" :defaults="capabilities.value.value?.projects.defaults" :disabled="action.locked.value || selectingLocation || capabilities.loading.value || !!capabilities.error.value || !capabilities.value.value" @selected="chooseLocation" />
  <RemoteState :loading="capabilities.loading.value" :error="capabilities.error.value" :has-data="!!capabilities.value.value" @retry="capabilities.refresh" />
  <p v-if="capabilities.value.value" class="text-xs text-muted-foreground">Codex {{ capabilities.value.value.status.nativeVersion }} · {{ capabilities.value.value.status.observedAt }} · Projects: {{ capabilities.value.value.projects.source }}</p>
  <Disclosure v-if="inspectPlan" title="Exact native plan"><pre class="max-h-80 overflow-auto whitespace-pre-wrap break-all rounded-lg border p-3 text-xs">{{ inspectPlan }}</pre></Disclosure>
  <Button type="button" :disabled="!canSave" @click="save">Save execution plan</Button><ActionState :action="action" @changed="recoveredPlan" />
</section></template>
