import type { Mermaid } from 'mermaid';

let mermaidRuntime: Promise<Mermaid> | null = null;
let diagramId = 0;

const loadMermaid = () => {
  mermaidRuntime ??= import('mermaid').then(({ default: mermaid }) => {
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: 'strict',
      suppressErrorRendering: true,
      layout: 'dagre',
      theme: 'neutral',
      look: 'classic',
      htmlLabels: false,
      maxTextSize: 20_000,
      maxEdges: 300,
    });
    return mermaid;
  });
  return mermaidRuntime;
};

const escapeHtml = (value: string) => value.replace(/[&<>"']/g, character => ({
  '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
})[character]!);

const safeSvg = (value: string) => {
  const document = new DOMParser().parseFromString(value, 'image/svg+xml');
  if (document.querySelector('parsererror') || document.documentElement.localName !== 'svg') throw new Error('Invalid SVG output.');
  document.querySelectorAll('script, foreignObject, image, iframe, object, embed').forEach(element => element.remove());
  document.querySelectorAll('*').forEach(element => {
    for (const attribute of [...element.attributes]) {
      const name = attribute.name.toLowerCase(), content = attribute.value.trim();
      if (name.startsWith('on') || ((name === 'href' || name === 'xlink:href') && content && !content.startsWith('#')) ||
        (name === 'style' && /(?:@import|url\(\s*["']?(?!#))/i.test(content))) element.removeAttribute(attribute.name);
    }
  });
  for (const style of document.querySelectorAll('style')) {
    if (/(?:@import|url\(\s*["']?(?!#))/i.test(style.textContent ?? '')) style.remove();
  }
  document.documentElement.setAttribute('role', 'img');
  document.documentElement.setAttribute('aria-label', 'Mermaid diagram');
  return new XMLSerializer().serializeToString(document.documentElement);
};

export const renderMermaidPreview = (language: string, source: string, applyPreview: (value: null | string | HTMLElement) => void) => {
  if (language.toLowerCase() !== 'mermaid' || !source.trim()) return null;
  if (/(?:https?|ftp|file|data|javascript|blob):|\/\//i.test(source)) {
    return '<p class="ivy-mermaid-error">External resources are not allowed in diagrams.</p>';
  }
  void loadMermaid()
    .then(async mermaid => {
      const { svg } = await mermaid.render(`ivy-mermaid-${++diagramId}`, source);
      applyPreview(`<div class="ivy-mermaid-preview">${safeSvg(svg)}</div>`);
    })
    .catch(cause => {
      const detail = cause instanceof Error ? cause.message : 'Unknown diagram error.';
      applyPreview(`<p class="ivy-mermaid-error">The diagram could not be rendered: ${escapeHtml(detail.slice(0, 300))}</p>`);
    });
  return undefined;
};
