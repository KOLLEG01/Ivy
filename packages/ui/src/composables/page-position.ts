import { ref } from 'vue';

/** A cursor belongs to one applied query, never to a draft filter or another workspace. */
export function usePagePosition(storageKey?: () => string) {
  const cursors = ref<Array<string | undefined>>([undefined]), page = ref(1);
  const restore = () => {
    cursors.value = [undefined]; page.value = 1;
    if (!storageKey) return;
    try {
      const saved = JSON.parse(sessionStorage.getItem(storageKey()) ?? 'null');
      if (saved && Array.isArray(saved.cursors) && saved.cursors.length <= 500 && Number.isInteger(saved.page) && saved.page >= 1 && saved.page <= saved.cursors.length && saved.cursors[0] === null && saved.cursors.every((cursor: unknown) => cursor === null || typeof cursor === 'string' && cursor.length <= 32768)) {
        cursors.value = saved.cursors.map((cursor: string | null) => cursor ?? undefined); page.value = saved.page;
      }
    } catch { /* An unavailable view cache must not block reads. */ }
  };
  const remember = () => { if (storageKey) try { sessionStorage.setItem(storageKey(), JSON.stringify({ cursors: cursors.value.slice(0, 500), page: page.value })); } catch { /* Pagination still works without session storage. */ } };
  restore(); return { cursors, page, restore, remember };
}
