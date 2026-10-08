<script setup lang="ts">
import { ref, watch } from 'vue';
import { Alert, AlertDescription, Button, Checkbox, Label, Textarea } from '@ivy/ui';
import { newOperationId, serviceTools } from '../../../packages/sdk/src/client.js';
import type { Agent, Operation } from '../../../packages/sdk/src/client.js';
import { client } from './runtime';

const props = defineProps<{ node: string; hostId: string | null; status?: Agent.EnvironmentStatus | undefined }>();
type Kind = 'mcp' | 'skills';
interface Editor { text: string; enabled: boolean; loaded: boolean; busy: boolean; error: string; message: string; object: Operation.ObjectMetadata | null; pending: Operation.ObjectsWriteParams | null }
const editors = ref<Record<Kind, Editor>>({
  mcp: { text: '[]', enabled: true, loaded: false, busy: false, error: '', message: '', object: null, pending: null },
  skills: { text: '[]', enabled: true, loaded: false, busy: false, error: '', message: '', object: null, pending: null },
});
const contracts = { mcp: 'agent/mcp-configuration', skills: 'agent/skills' } as const;
async function name(kind: Kind, hostId: string | null) {
  const base = kind === 'mcp' ? 'ivy-agent-mcp' : 'ivy-agent-skills';
  if (hostId === null) return base;
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(hostId));
  return base + '-host-' + [...new Uint8Array(bytes)].map(value => value.toString(16).padStart(2, '0')).join('');
}
async function read(kind: Kind, hostId: string | null): Promise<{ object: Operation.ObjectMetadata; document: Agent.McpDocument | Agent.SkillsDocument } | null> {
  let object: Operation.ObjectMetadata;
  try { object = await client.request('objects.stat', { path: '/' + await name(kind, hostId) }); }
  catch (failure) { if ((failure as { code?: string }).code === 'not_found') return null; throw failure; }
  const value = await client.request('objects.read', { objectId: object.id, revision: object.currentRevision });
  if (value.object.contractKey !== contracts[kind] || value.revision.contractVersion !== '1.0.0' || value.content.encoding !== 'json') throw new Error('The managed environment path contains another document type.');
  return { object, document: value.content.value as Agent.McpDocument | Agent.SkillsDocument };
}
async function load(kind: Kind) {
  const editor = editors.value[kind]; if (editor.busy) return;
  editor.busy = true; editor.error = ''; editor.loaded = false;
  try {
    const own = await read(kind, props.hostId);
    const defaults = await serviceTools(client, props.node, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).call('agent.environmentDefaults', {}) as Agent.EnvironmentDefaults;
    const inherited = props.hostId === null ? null : await read(kind, null);
    const source = own?.document ?? (inherited?.document.enabled === null ? defaults[kind] : inherited?.document) ?? defaults[kind];
    if (source.hostId !== (own ? props.hostId : source.hostId)) throw new Error('Managed environment scope does not match.');
    editor.object = own?.object ?? null; editor.enabled = own ? own.document.enabled === true : true;
    editor.text = JSON.stringify(kind === 'mcp' ? (source as Agent.McpDocument).servers : (source as Agent.SkillsDocument).files, null, 2);
    editor.message = own ? own.document.enabled === null ? 'No override for this scope; inherited configuration applies.' : `Saved revision ${own.object.currentRevision}.`
      : props.hostId === null ? 'Project defaults are active until you save a Hive-wide override.' : 'Inherited Hive-wide configuration is active.';
    editor.loaded = true;
  } catch (failure) { editor.error = failure instanceof Error ? failure.message : String(failure); }
  finally { editor.busy = false; }
}
async function save(kind: Kind, remove = false) {
  const editor = editors.value[kind]; if (editor.busy || !editor.loaded) return;
  editor.busy = true; editor.error = '';
  try {
    let values: unknown = [];
    if (!remove) { values = JSON.parse(editor.text); if (!Array.isArray(values)) throw new Error('Configuration must be a JSON array.'); }
    editor.pending ??= { mutationId: await newOperationId(client), contractVersion: '1.0.0', references: {},
      content: { encoding: 'json', value: kind === 'mcp'
        ? { schemaVersion: 1, hostId: props.hostId, enabled: remove ? null : editor.enabled, servers: remove ? [] : values }
        : { schemaVersion: 1, hostId: props.hostId, enabled: remove ? null : editor.enabled, files: remove ? [] : values } },
      ...(editor.object ? { objectId: editor.object.id, expectedRevision: editor.object.currentRevision }
        : { create: { contractKey: contracts[kind], parentId: null, ownerObjectId: null, name: await name(kind, props.hostId) } }) };
    const result = await client.request('objects.write', editor.pending); editor.object = result.object; editor.pending = null;
    editor.message = `Saved revision ${result.revision.revision}. Hosts synchronize automatically; new tasks use the updated environment.`;
  } catch (failure) {
    const code = (failure as { code?: string }).code;
    if (['revision_conflict', 'name_conflict', 'invalid_arguments', 'contract_mismatch', 'forbidden'].includes(code ?? '')) editor.pending = null;
    editor.error = failure instanceof Error ? failure.message : String(failure);
  } finally { editor.busy = false; }
}
watch(() => [props.node, props.hostId], () => { for (const kind of ['mcp', 'skills'] as const) { editors.value[kind].pending = null; void load(kind); } }, { immediate: true });
</script>

<template><section class="space-y-5" :aria-label="hostId === null ? 'Hive agent environment' : 'Host agent environment'">
  <h3 class="sr-only">{{ hostId === null ? 'Hive-wide MCP and skills' : 'MCP and skills for this host' }}</h3>
  <p class="text-sm text-muted-foreground">Secrets are never stored in these documents. AgentManager injects its own provisioned key only for the exact Hive MCP URL.</p>
  <section v-for="kind in (['mcp', 'skills'] as const)" :key="kind" class="space-y-3 border-t pt-4" :aria-label="kind === 'mcp' ? 'MCP configuration' : 'Skills configuration'">
    <h4 class="text-sm font-medium">{{ kind === 'mcp' ? 'MCP servers' : 'Skill files' }}</h4>
    <fieldset :disabled="editors[kind].busy || !editors[kind].loaded || !!editors[kind].pending" class="min-w-0 space-y-3">
      <Label class="flex items-center gap-2"><Checkbox v-model="editors[kind].enabled" />Enable this configuration</Label>
      <Label class="min-w-0 flex-col items-start gap-2">{{ kind === 'mcp' ? 'Server definitions (JSON)' : 'Files (JSON)' }}<Textarea v-model="editors[kind].text" :rows="kind === 'mcp' ? 12 : 18" maxlength="4194304" class="max-h-96 min-w-0 font-mono text-xs font-normal [field-sizing:fixed]" /></Label>
    </fieldset>
    <div class="flex flex-wrap gap-2"><Button :disabled="editors[kind].busy || !editors[kind].loaded" @click="save(kind)">{{ editors[kind].pending ? 'Retry original save' : 'Save' }}</Button>
      <Button variant="outline" :disabled="editors[kind].busy || !editors[kind].loaded || !!editors[kind].pending || !editors[kind].object" @click="save(kind, true)">Remove this override</Button>
      <Button variant="ghost" :disabled="editors[kind].busy || !!editors[kind].pending" @click="load(kind)">Reload</Button></div>
    <Alert v-if="editors[kind].error" variant="destructive"><AlertDescription>{{ editors[kind].error }}</AlertDescription></Alert><p v-if="editors[kind].message" role="status" class="text-sm">{{ editors[kind].message }}</p>
    <div v-if="status" class="text-xs text-muted-foreground"><p>Local application: {{ status[kind].state }}{{ status[kind].code ? ' · ' + status[kind].code : '' }}</p><p class="break-all">Target: {{ status[kind].target }}</p></div>
  </section>
  <p class="text-xs text-muted-foreground">MCP tables are isolated in a marked config.toml block. Skills are installed as Ivy-owned directories below .agents/skills. Local edits to Ivy-owned output stop replacement instead of being overwritten.</p>
</section></template>
