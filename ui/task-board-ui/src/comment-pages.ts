import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue';
import type { Ref } from 'vue';
import type { TaskBoard } from './runtime';

/** Keep one history page beside the current snapshot, never the pages traversed to reach it. */
export function useCommentPages(current: Ref<TaskBoard.TaskView | null>,
  read: (before: number, signal: AbortSignal) => Promise<TaskBoard.TaskView>, follow = () => true) {
  const pageSize = 50;
  const history = shallowRef<TaskBoard.Comment[] | null>(null);
  const loading = ref(false), error = ref<string | null>(null);
  const latestComments = computed(() => [...current.value?.task.comments ?? []].sort((a, b) => a.sequence - b.sequence));
  const comments = computed(() => history.value ?? latestComments.value);
  const total = computed(() => current.value?.commentPage?.total ?? 0);
  const first = computed(() => comments.value[0]?.sequence ?? 0);
  const last = computed(() => comments.value.at(-1)?.sequence ?? 0);
  watch(current, (view, previous) => {
    if (previous && !history.value && !follow() && (view?.commentPage?.total ?? 0) > (previous.commentPage?.total ?? 0))
      history.value = [...previous.task.comments].sort((a, b) => a.sequence - b.sequence);
  }, { flush: 'sync' });
  let controller: AbortController | undefined;
  const cancel = () => { controller?.abort(); controller = undefined; loading.value = false; };
  onBeforeUnmount(cancel);
  const latest = () => { cancel(); history.value = null; error.value = null; };
  const load = async (before: number, find?: string): Promise<boolean> => {
    cancel();
    const work = controller = new AbortController();
    loading.value = true; error.value = null;
    try {
      while (before > 1) {
        const view = await read(before, work.signal);
        work.signal.throwIfAborted();
        const page = [...view.task.comments].sort((a, b) => a.sequence - b.sequence);
        if (!find || page.some(comment => comment.commentId === find)) {
          history.value = page;
          return true;
        }
        const next = page[0]?.sequence ?? 0;
        if (!next || next >= before) break;
        before = next;
      }
      if (find) error.value = 'The message could not be found.';
      return false;
    } catch (cause) {
      if (!work.signal.aborted) error.value = cause instanceof Error ? cause.message : String(cause);
      return false;
    } finally {
      if (controller === work) { controller = undefined; loading.value = false; }
    }
  };
  const older = () => first.value > 1 ? load(first.value) : Promise.resolve(false);
  const newer = () => {
    const before = last.value + pageSize + 1;
    if (before > total.value) { latest(); return Promise.resolve(true); }
    return load(before);
  };
  const find = async (id: string, sequence?: number) => {
    if (comments.value.some(comment => comment.commentId === id)) return true;
    if (current.value?.task.comments.some(comment => comment.commentId === id)) { latest(); return true; }
    // Questions already include their sequence. Unknown deep links search one bounded
    // page at a time; cancellation or failure leaves the selected page intact.
    return load(sequence ? Math.min(total.value + 1, sequence + 25) : total.value + 1, id);
  };
  return { comments, latestComments, total, first, last, loading, error, latest, older, newer, find,
    historical: computed(() => history.value !== null),
    hasOlder: computed(() => first.value > 1), hasNewer: computed(() => last.value < total.value) };
}
