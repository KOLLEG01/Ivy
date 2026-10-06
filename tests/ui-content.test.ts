import test from 'node:test';
import assert from 'node:assert/strict';
import { markdownBlocks, renderMarkdown } from '../packages/ui/src/lib/markdown.js';
import { definedProps } from '../packages/ui/src/lib/utils.js';
import { attachmentMarkdown } from '../packages/ui/src/lib/attachments.js';

test('verified attachments render safe image previews and file cards without interpreting filenames as Markdown', () => {
  const src = '#/page?id=file&revision=1', name = 'report [draft] \\ _notes_ <img>.pdf';
  const file = attachmentMarkdown({ src, name, image: false });
  const rendered = renderMarkdown(file, 262144, {}, { [src]: { name, byteLength: 1234 } });
  assert.match(rendered.html, /class="ivy-attachment-link"/);
  assert.match(rendered.html, /data-file-size="1.2 KiB"/);
  assert.doesNotMatch(rendered.html, /<img|<em>/);
  assert.match(rendered.html, /report \[draft\]/);
  const image = renderMarkdown('![A](#/page?id=file&revision=1)', 262144, { [src]: 'blob:verified' });
  assert.match(image.html, /aria-label="Enlarge A"/);
  assert.match(image.html, /src="blob:verified"/);
  assert.doesNotMatch(renderMarkdown('![A](' + src + ')', 262144, { [src]: 'https://tracker.test/image' }).html, /<img/);
});

test('untrusted Markdown emits no active HTML, unsafe URL or automatically fetched image', () => {
  const source = '# Saved result\n\n<script>alert(1)</script>\n\n<iframe src="https://tracker.test"></iframe>\n\n'
    + '<svg onload="alert(1)"></svg>\n\n[x](javascript:alert%281%29)\n\n[x](jav&#x61;script:alert%281%29)\n\n'
    + '[x](data:text/html,attack)\n\n![private image](https://tracker.test/pixel?secret=content)\n\n'
    + '[Evidence](https://example.test/result)';
  const result = renderMarkdown(source);
  assert.match(result.html, /<h1>Saved result<\/h1>/);
  assert.doesNotMatch(result.html, /<(script|iframe|svg|img)(\s|>)/i);
  assert.doesNotMatch(result.html, /href="(?:javascript|data):/i);
  assert.ok(!result.html.includes('pixel?secret=content'));
  assert.match(result.html, /href="https:\/\/example.test\/result" rel="noreferrer noopener"/);
  assert.equal(result.truncated, false);
  assert.equal(renderMarkdown('x'.repeat(300000)).truncated, true);
  assert.ok(renderMarkdown('x'.repeat(300000)).html.length < 263000);
});

test('primitive forwarding keeps falsy values and required values while removing undefined without mutating the source', () => {
  const listener = () => undefined;
  const original = { value: 'selected', disabled: false, count: 0, text: '', nullable: null, absent: undefined, onSelect: listener };
  const forwarded = definedProps(original);
  assert.deepEqual(forwarded, { value: 'selected', disabled: false, count: 0, text: '', nullable: null, onSelect: listener });
  assert.ok(Object.hasOwn(original, 'absent'));
  const required: string = forwarded.value; assert.equal(required, 'selected');
});

test('document blocks preserve exact Markdown, including opaque syntax, tables, whitespace and fenced blank lines', () => {
  for (const source of ['', '\n\n# Title\r\n\r\nParagraph with **bold**.\r\n', '- One\n- Two\n\n> A quote\n> More\n', '~~~js\nconst x = 1;\n\n// inside code\n~~~\n\n| A | B |\n| - | - |\n| 1 | 2 |\n', '<custom-tag>keep this</custom-tag>\n\n![alt](https://example.test/image)\n\n[ref]: /relative\n']) {
    const blocks = markdownBlocks(source);
    assert.equal(blocks.map(block => block.content + block.separator).join(''), source);
  }
  const fenced = markdownBlocks('Before\n\n~~~\nA\n\nB\n~~~\n\nAfter');
  assert.equal(fenced.length, 3); assert.ok(fenced[1]!.content.includes('A\n\nB'));
});
