/** Keep a visible row in place when content is inserted above it, including inside dialogs. */
export function preserveScrollAnchor(element: HTMLElement | undefined): () => void {
  if (!element) return () => {};
  const top = element.getBoundingClientRect().top;
  let parent = element.parentElement;
  while (parent && !(/auto|scroll/.test(getComputedStyle(parent).overflowY) && parent.scrollHeight > parent.clientHeight))
    parent = parent.parentElement;
  const scroller = parent ?? window;
  return () => {
    if (element.isConnected) scroller.scrollBy({ top: element.getBoundingClientRect().top - top, behavior: 'instant' });
  };
}
