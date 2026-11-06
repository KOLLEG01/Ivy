import { createApp, h, ref } from 'vue';
import { IvyShell } from '@ivy/ui';
import '@ivy/ui/styles.css';
import TaskEditor from '../../../ui/task-board-ui/src/TaskEditor.vue';
import SettingsView from '../../../ui/task-board-ui/src/SettingsView.vue';
import TaskDetail from '../../../ui/task-board-ui/src/TaskDetail.vue';
import HostDirectoryDialog from '../../../packages/ui-client/src/HostDirectoryDialog.vue';
import NativeInputs from '../../../packages/ui-client/src/NativeInputs.vue';
import { backends, fixtureClient, fixtureNotifications, hostId, serviceNodeId, state, task, workspace } from './execution-rpc.mjs';
const mode = new URLSearchParams(location.search).get('mode');
if (mode?.startsWith('linked')) Object.assign(task, { lastRun: { objectId: 'run', revision: 1 },
  primaryResourceRef: { serviceNodeId, namespace: backends[1].backendId, kind: 'thread', nativeId: 'retained' },
  fields: { ...task.fields, nativeOptions: { backend: backends[0] } } });
createApp({ setup() {
  const backend = ref(backends[0]), open = ref(true);
  state.changeDirectoryBackend = () => { backend.value = backends[1]; };
  const content = () => mode?.startsWith('input') ? h(NativeInputs, { client: fixtureClient, notifications: fixtureNotifications, base: new URL(location.origin),
    node: serviceNodeId, backend: backends[1], threadId: 'same-thread', turnId: 'same-turn', ownerEpoch: 'secondary-epoch', retainDrafts: true }) :
    mode === 'directory' ? h(HostDirectoryDialog, { client: fixtureClient, node: serviceNodeId, host: hostId,
    backend: backend.value, start: '/work', open: open.value, 'onUpdate:open': value => { open.value = value; }, onSelect: path => { state.selectedDirectory = path; } }) :
    mode === 'settings' ? h(SettingsView, { workspace, available: true }) :
    mode === 'detail' || mode?.startsWith('linked') ? h(TaskDetail, { workspace, available: true, taskId: 'task', query: new URLSearchParams('tab=details') }) :
    h(TaskEditor, { workspace, available: true, ...(mode === 'placed' ? { task: { value: { ...task, lastRun: { objectId: 'run', revision: 1 },
      primaryResourceRef: { serviceNodeId, namespace: backends[1].backendId, kind: 'thread', nativeId: 'retained' } }, pin: { objectId: 'task', revision: 1 } } } : {}) });
  return () => h(IvyShell, { uiName: 'TaskBoard', homeUrl: '/', logoutUrl: '/logout', navigation: [], connection: 'Fixture', layout: 'default' },
    { default: () => h('div', { class: 'mx-auto max-w-5xl p-4' }, [content()]) });
} }).mount('#ui');
