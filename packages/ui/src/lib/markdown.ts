import MarkdownIt from 'markdown-it';
import { fileSize } from './attachments.js';
import type { AttachmentInfo } from './attachments.js';

// Raw HTML is text. Only blob URLs supplied after attachment verification may render as images.
const markdown = new MarkdownIt({ html: false, linkify: false, typographer: false, maxNesting: 32 });
markdown.renderer.rules['image'] = (tokens, index, _options, env) => {
  const token = tokens[index]!, source = token.attrGet('src') ?? '';
  const verified = (env as { trustedImageUrls?: Record<string, string> }).trustedImageUrls?.[source];
  if (!verified?.startsWith('blob:')) return `<span class="ivy-image-reference">[${markdown.utils.escapeHtml(token.content || 'Image')}]</span>`;
  return `<button type="button" class="ivy-image-button" aria-label="${markdown.utils.escapeHtml('Enlarge ' + token.content)}"><img class="ivy-embedded-image" src="${markdown.utils.escapeHtml(verified)}" alt="${markdown.utils.escapeHtml(token.content)}" loading="lazy"></button>`;
};
const defaultLink = markdown.renderer.rules['link_open'];
markdown.renderer.rules['link_open'] = (tokens, index, options, env, renderer) => {
  const token = tokens[index]!;
  token.attrSet('rel', 'noreferrer noopener');
  const attachment = (env as { trustedAttachments?: Record<string, AttachmentInfo> }).trustedAttachments?.[token.attrGet('href') ?? ''];
  if (attachment) { token.attrSet('class', 'ivy-attachment-link'); token.attrSet('data-file-size', fileSize(attachment.byteLength)); token.attrSet('title', attachment.name + ' · ' + fileSize(attachment.byteLength)); }
  return defaultLink ? defaultLink(tokens, index, options, env, renderer) : renderer.renderToken(tokens, index, options);
};
export function renderMarkdown(source: string, maximumCharacters = 262144, trustedImageUrls: Record<string, string> = {}, trustedAttachments: Record<string, AttachmentInfo> = {}): { html: string; truncated: boolean } {
  return { html: markdown.render(source.slice(0, maximumCharacters), { trustedImageUrls, trustedAttachments }), truncated: source.length > maximumCharacters };
}

// Top-level token boundaries keep lists, fenced code and tables together. Retain whitespace and
// unsupported syntax verbatim: opening the editor must never rewrite the stored document.
export function markdownBlocks(source: string): Array<{ content: string; separator: string }> {
  const lines = source.split(/(?<=\n)/), offsets = [0];
  for (const line of lines) offsets.push(offsets.at(-1)! + line.length);
  const starts = [...new Set([0, ...markdown.parse(source, {}).filter(token => token.level === 0 && token.map).map(token => offsets[token.map![0]]!).filter(offset => offset > 0)])].sort((a, b) => a - b);
  return starts.map((start, index) => { const raw = source.slice(start, starts[index + 1] ?? source.length), content = raw.trimEnd(); return { content, separator: raw.slice(content.length) }; });
}
