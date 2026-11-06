<script setup lang="ts">
import { computed, ref, shallowRef, watch } from 'vue';
import { Button, Label, OptionSelect, RemoteState, useRemote } from '@ivy/ui';
import type { Agent, Operation } from '../../../packages/sdk/src/client.js';
import { newOperationId } from '../../../packages/sdk/src/client.js';
import { nativeBackendLabel, nativeBackendScopes, nativeMethod } from '../../../packages/ui-client/src/native-backends';
import { nativeHosts, nativeServiceNodes } from '../../../packages/ui-client/src/native-hosts';
import { nativeOwnerKey } from '../../../packages/ui-client/src/native-route';
import { nativeModelSpeedAvailable, nativeSpeedAvailable } from '../../../packages/ui-client/src/execution-models';
import { optionalTool, permissionProfilesFrom, permissionProfileLabel } from '../../../packages/ui-client/src/native';
import { nativeModesFrom } from '../../../packages/ui-client/src/native-modes';
import { effectiveNativeSettings, nativeConfiguredHome, useNativeSettings } from '../../../packages/ui-client/src/native-settings';
import { accountExecutionDefaultsPath, emptyAccountExecutionDefaults, readExecutionDefaults, withAccountExecutionProfile } from '../../../packages/ui-client/src/execution-defaults';
import { client, tr } from './runtime';

const discovery = useRemote(async signal => {
  const nodes = await nativeServiceNodes(client, signal);
  const results = await Promise.all(nodes.filter(node => node.connected).map(async node => {
    try { return { scopes: await nativeBackendScopes(client, node, signal), error: '' }; }
    catch (error) { signal.throwIfAborted(); return { scopes: [], error: `${node.hostId}: ${error instanceof Error ? error.message : String(error)}` }; }
  }));
  return { nodes, scopes: results.flatMap(result => result.scopes), errors: results.map(result => result.error).filter(Boolean) };
}, 30000, ['services/agent-manager']);
const hosts = computed(() => nativeHosts(discovery.value.value?.nodes ?? []));
const host = ref(''), account = ref('');
const ownerKey = (scope: { serviceNodeId: string; backend?: Agent.BackendIdentity }) => nativeOwnerKey({ ...scope, namespace: scope.backend?.backendId ?? 'codex' });
const accounts = computed(() => discovery.value.value?.scopes.filter(scope => scope.hostId === host.value && scope.backend) ?? []);
const selected = computed(() => accounts.value.find(scope => ownerKey(scope) === account.value));
const selectedBackend = computed(() => selected.value?.backend);
const settings = useNativeSettings(client, () => selected.value?.serviceNodeId ?? '', () => nativeConfiguredHome(selected.value?.status as Agent.Status | undefined), () => selectedBackend.value);
const tools = useRemote(async () => selected.value ? Promise.all(['thread/start', 'turn/start'].map(method => optionalTool(client, selected.value!.serviceNodeId, nativeMethod(selectedBackend.value, method)))) : [], 0, ['services/agent-manager']);
watch(account, () => { tools.value.value = null; void tools.refresh(); });
const legacy = useRemote(signal => readExecutionDefaults(client, signal), 0, ['objects/agent/execution-defaults']);
const profile = ref(emptyAccountExecutionDefaults());
const dirty = ref(false), saving = ref(false), error = ref(''), message = ref('');
const pending = ref<Operation.ObjectsWriteParams | null>(null);
const editBase = shallowRef<typeof settings.defaults.value.value | undefined>();
const beginEdit = () => { if (!dirty.value) editBase.value = settings.defaults.value.value; dirty.value = true; message.value = ''; };
// A draft belongs to its full identity; changing hosts/accounts explicitly starts another edit.
const selectHost = (value: string) => { if (saving.value || pending.value) return; host.value = value; selectAccount(''); };
const selectAccount = (value: string) => { if (saving.value || pending.value) return; account.value = value; dirty.value = false; editBase.value = undefined; error.value = message.value = ''; profile.value = structuredClone(settings.profile.value ?? emptyAccountExecutionDefaults()); };
watch(() => settings.profile.value, value => {
  if (!dirty.value && !pending.value) profile.value = structuredClone(value ?? emptyAccountExecutionDefaults());
});
const inherited = computed(() => effectiveNativeSettings(settings.models.value.value, settings.config.value.value, null));
const model = computed(() => profile.value.model || inherited.value.model);
const inheritedEffort = computed(() => effectiveNativeSettings(settings.models.value.value, settings.config.value.value, null, { model: profile.value.model }).effort);
const efforts = computed(() => settings.choices.value.find(choice => choice.model === model.value)?.efforts ?? []);
const modeChoices = computed(() => nativeModesFrom(tools.value.value?.[1], settings.modes.value.value));
const permissionChoices = computed(() => permissionProfilesFrom(settings.permissions.value.value).filter(choice => choice.allowed));
const speedAvailable = (tier: 'standard' | 'fast' | 'flex') => nativeModelSpeedAvailable(settings.choices.value, model.value, tier) && nativeSpeedAvailable(tools.value.value ?? [], tier, selectedBackend.value?.provider);
const previous = computed(() => selectedBackend.value ? legacy.value.value?.document[selectedBackend.value.provider] : null);
const overridden = computed(() => Object.values(profile.value).some(value => value !== null));
const invalid = computed(() => !selectedBackend.value || selected.value?.status.state !== 'ready' ||
  !!settings.models.error.value || !!settings.config.error.value || !!settings.modes.error.value || !!settings.permissions.error.value || !!tools.error.value ||
  settings.models.loading.value || settings.config.loading.value || settings.permissions.loading.value || tools.loading.value ||
  !!profile.value.model && !settings.choices.value.some(choice => choice.model === profile.value.model) ||
  !!profile.value.effort && !efforts.value.includes(profile.value.effort) ||
  !!profile.value.mode && !modeChoices.value.some(choice => choice.mode === profile.value.mode) ||
  !!profile.value.permission && !permissionChoices.value.some(choice => choice.id === profile.value.permission) ||
  !!profile.value.serviceTier && !speedAvailable(profile.value.serviceTier));
const set = (field: keyof Agent.AccountExecutionDefaults, value: string) => {
  beginEdit();
  if (field === 'serviceTier') profile.value.serviceTier = value === 'fast' || value === 'flex' || value === 'standard' ? value : null;
  else profile.value[field] = value || null;
  if (field === 'model' && profile.value.effort && !efforts.value.includes(profile.value.effort)) profile.value.effort = null;
};
const inheritAll = () => { beginEdit(); profile.value = emptyAccountExecutionDefaults(); };
const copyPrevious = () => { if (previous.value) { beginEdit(); profile.value = { ...previous.value, serviceTier: null }; } };
const discard = () => { dirty.value = false; editBase.value = undefined; profile.value = structuredClone(settings.profile.value ?? emptyAccountExecutionDefaults()); error.value = message.value = ''; };
async function save() {
  if (saving.value || !pending.value && (invalid.value || !dirty.value || settings.defaults.loading.value || settings.defaults.error.value)) return;
  saving.value = true; error.value = '';
  try {
    if (!pending.value) {
      const current = editBase.value === undefined ? settings.defaults.value.value : editBase.value, backend = selectedBackend.value!;
      const document = withAccountExecutionProfile(current?.document, backend, profile.value);
      const mutationId = await newOperationId(client);
      pending.value = { mutationId, contractVersion: '1.0.0', references: {}, content: { encoding: 'json', value: document },
        ...(current ? { objectId: current.object.id, expectedRevision: current.object.currentRevision } :
          { create: { contractKey: 'agent/account-execution-defaults', parentId: null, ownerObjectId: null, name: accountExecutionDefaultsPath.slice(1) } }) };
    }
    await client.request('objects.write', pending.value);
    pending.value = null; dirty.value = false; editBase.value = undefined;
    message.value = tr('Gespeichert. Neue Tasks dieses Accounts verwenden die Vorgaben.', 'Saved. New tasks for this account use these defaults.');
    await settings.defaults.refresh();
  } catch (cause) {
    if (['revision_conflict', 'name_conflict', 'invalid_arguments', 'contract_mismatch', 'forbidden'].includes((cause as { code?: string }).code ?? '')) {
      pending.value = null;
      await settings.defaults.refresh(); // Keep the draft; the next explicit save uses the current CAS revision.
      editBase.value = settings.defaults.value.value;
    }
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally { saving.value = false; }
}
</script>

<template>
  <div class="space-y-4">
    <p class="text-sm text-muted-foreground">{{ tr('Vorgaben gelten für die vollständige Provider-/Account-Auswahl auf allen Hosts. Leere Werte folgen der nativen Konfiguration. Bestehende Tasks behalten ihre Einstellungen.', 'Defaults apply to the complete provider and account selection across hosts. Inherited values follow its native configuration. Existing tasks retain their settings.') }}</p>
    <RemoteState :loading="discovery.loading.value || settings.defaults.loading.value" :error="discovery.error.value || settings.defaults.error.value" :has-data="!!discovery.value.value" @retry="discovery.refresh(); settings.refresh()" />
    <div class="grid min-w-0 gap-3 sm:grid-cols-2">
      <Label class="flex-col items-start gap-2">{{ tr('Auswahl laut Host', 'Choices from host') }}
        <OptionSelect :model-value="host" aria-label="Defaults host" :disabled="saving || !!pending" @update:model-value="selectHost(String($event))">
          <option value="" disabled>{{ tr('Host auswählen', 'Choose a host') }}</option>
          <option v-for="item in hosts" :key="item.hostId" :value="item.hostId">{{ item.hostId }}</option>
        </OptionSelect>
      </Label>
      <Label class="flex-col items-start gap-2">{{ tr('Provider und Account', 'Provider and account') }}
        <OptionSelect :model-value="account" aria-label="Defaults account" :disabled="!host || saving || !!pending" @update:model-value="selectAccount(String($event))">
          <option value="" disabled>{{ tr('Account auswählen', 'Choose an account') }}</option>
          <option v-for="scope in accounts" :key="ownerKey(scope)" :value="ownerKey(scope)">{{ nativeBackendLabel(scope.backend!) }}{{ scope.status.state === 'ready' ? '' : ' · ' + scope.status.state }}</option>
        </OptionSelect>
      </Label>
    </div>
    <p v-for="problem in discovery.value.value?.errors" :key="problem" class="text-sm text-muted-foreground">{{ problem }}</p>
    <p v-if="account && !selected" role="alert" class="text-sm text-destructive">{{ tr('Der ursprüngliche Account ist nicht verfügbar. Der Entwurf bleibt erhalten.', 'The original account is unavailable. The draft is retained.') }}</p>
    <fieldset v-if="selected" :disabled="saving || !!pending || settings.defaults.loading.value" class="grid min-w-0 gap-3 sm:grid-cols-2">
      <Label class="flex-col items-start gap-2">{{ tr('Modell', 'Model') }}
        <OptionSelect :model-value="profile.model || ''" aria-label="Default model for account" @update:model-value="set('model', String($event))">
          <option value="">{{ inherited.model || tr('Servervorgabe', 'Server setting') }} · {{ tr('vom Server', 'from server') }}</option>
          <option v-if="profile.model && !settings.choices.value.some(choice => choice.model === profile.model)" :value="profile.model">{{ profile.model }} (unavailable)</option>
          <option v-for="choice in settings.choices.value" :key="choice.id" :value="choice.model">{{ choice.name }}</option>
        </OptionSelect>
      </Label>
      <Label class="flex-col items-start gap-2">{{ tr('Arbeitsmodus', 'Working mode') }}
        <OptionSelect :model-value="profile.mode || ''" aria-label="Default mode for account" @update:model-value="set('mode', String($event))">
          <option value="">{{ modeChoices.find(choice => choice.mode === inherited.mode)?.name || inherited.mode }} · {{ tr('vom Server', 'from server') }}</option>
          <option v-if="profile.mode && !modeChoices.some(choice => choice.mode === profile.mode)" :value="profile.mode">{{ profile.mode }} (unavailable)</option>
          <option v-for="choice in modeChoices" :key="choice.mode" :value="choice.mode">{{ choice.name }}</option>
        </OptionSelect>
      </Label>
      <Label class="flex-col items-start gap-2">{{ tr('Denkaufwand', 'Reasoning effort') }}
        <OptionSelect :model-value="profile.effort || ''" aria-label="Default effort for account" @update:model-value="set('effort', String($event))">
          <option value="">{{ inheritedEffort || tr('Kein Denkaufwand', 'No reasoning effort') }} · {{ tr('vom Server', 'from server') }}</option>
          <option v-if="profile.effort && !efforts.includes(profile.effort)" :value="profile.effort">{{ profile.effort }} (unavailable)</option>
          <option v-for="choice in efforts" :key="choice" :value="choice">{{ choice }}</option>
        </OptionSelect>
      </Label>
      <Label class="flex-col items-start gap-2">{{ tr('Berechtigungen', 'Safety') }}
        <OptionSelect :model-value="profile.permission || ''" aria-label="Default safety for account" @update:model-value="set('permission', String($event))">
          <option value="">{{ inherited.permission ? permissionProfileLabel(inherited.permission) : tr('Servervorgabe', 'Server setting') }} · {{ tr('vom Server', 'from server') }}</option>
          <option v-if="profile.permission && !permissionChoices.some(choice => choice.id === profile.permission)" :value="profile.permission">{{ profile.permission }} (unavailable)</option>
          <option v-for="choice in permissionChoices" :key="choice.id" :value="choice.id">{{ permissionProfileLabel(choice.id) }}</option>
        </OptionSelect>
      </Label>
      <Label class="flex-col items-start gap-2">Speed
        <OptionSelect :model-value="profile.serviceTier || ''" aria-label="Default speed for account" @update:model-value="set('serviceTier', String($event))">
          <option value="">{{ inherited.serviceTier }} · {{ tr('vom Server', 'from server') }}</option>
          <option v-for="tier in ['standard', 'fast', 'flex'] as const" :key="tier" :value="tier" :disabled="!speedAvailable(tier)">{{ tier === 'flex' ? 'Flexible' : tier === 'fast' ? 'Fast' : 'Standard' }}</option>
        </OptionSelect>
      </Label>
    </fieldset>
    <p v-if="settings.models.error.value || settings.config.error.value || settings.modes.error.value || settings.permissions.error.value || tools.error.value" role="alert" class="text-sm text-destructive">{{ settings.models.error.value || settings.config.error.value || settings.modes.error.value || settings.permissions.error.value || tools.error.value }}</p>
    <div v-if="selected" class="flex flex-wrap gap-2">
      <Button v-if="overridden" variant="link" size="sm" :disabled="saving || !!pending" @click="inheritAll">{{ tr('Alles vom Server übernehmen', 'Inherit everything from the server') }}</Button>
      <Button v-if="dirty" variant="link" size="sm" :disabled="saving || !!pending" @click="discard">{{ tr('Änderungen verwerfen', 'Discard changes') }}</Button>
      <Button v-if="previous" variant="link" size="sm" :disabled="saving || !!pending" @click="copyPrevious">{{ tr('Bisherige Provider-Vorgaben kopieren', 'Copy previous provider defaults') }}</Button>
    </div>
    <div class="flex gap-2">
      <Button :disabled="saving || (!pending && (!dirty || invalid || settings.defaults.loading.value || !!settings.defaults.error.value))" @click="save">{{ pending ? tr('Speichern erneut versuchen', 'Retry original save') : tr('Defaults speichern', 'Save defaults') }}</Button>
      <Button variant="ghost" :disabled="saving || !!pending" @click="discovery.refresh(); settings.refresh()">{{ tr('Neu laden', 'Reload') }}</Button>
    </div>
    <p v-if="error" role="alert" class="text-sm text-destructive">{{ error }}</p>
    <p v-if="message" role="status" class="text-sm">{{ message }}</p>
  </div>
</template>
