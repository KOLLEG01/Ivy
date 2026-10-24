import { inject, provide, ref, watch } from 'vue';
import type { InjectionKey, Ref } from 'vue';
import { base } from './runtime';
import type { TaskBoard } from './runtime';

const visibilityKey: InjectionKey<Ref<boolean>> = Symbol('task-board-cross-agent');
export function provideCommentVisibility(workspace: () => TaskBoard.WorkspaceInfo | null) {
  const showCrossAgent = ref(false);
  const key = () => {
    const value = workspace();
    return value ? 'ivy.task-board.cross-agent:' + JSON.stringify([base.href, value.serviceNodeId,
      value.callerPrincipalId, value.principalId, value.rootObjectId]) : '';
  };
  watch(key, value => {
    try { showCrossAgent.value = !!value && localStorage.getItem(value) === 'true'; }
    catch { showCrossAgent.value = false; }
  }, { immediate: true });
  watch(showCrossAgent, value => {
    try { if (key()) localStorage.setItem(key(), String(value)); }
    catch { /* The current view still works without optional preference storage. */ }
  });
  provide(visibilityKey, showCrossAgent);
  return showCrossAgent;
}
export const useCommentVisibility = () => inject(visibilityKey, ref(false));
