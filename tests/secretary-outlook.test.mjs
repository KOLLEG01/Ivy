import test from 'node:test';
import assert from 'node:assert/strict';
import { hashJson } from '../dist/packages/sdk/src/node.js';
import { microsoftMcp, microsoftProfile, outlookArguments, outlookBody, outlookPage } from '../dist/services/secretary/src/outlook-projection.js';
import { validateMicrosoft } from '../dist/services/secretary/src/microsoft-schema.js';

const profile = { id: 'account-one', email: 'me@example.test' };
const source = { sourceId: 'mail', kind: 'email', accountId: profile.id, producerPrincipalIds: ['producer'], allowedSenderIds: null, ownSenderIds: ['me@example.test'] };
const selected = { since: '2026-09-01T00:00:00.000Z', until: '2026-09-08T04:00:00.000Z', skip: 0, pageSize: 8 };
const observed = '2026-09-08T04:00:01.000Z';
const message = (changes = {}) => ({ id: 'native-message', sender: { emailAddress: { address: 'Sender@Example.test' } }, subject: 'Original title', receivedDateTime: '2026-09-07T23:00:00Z', isRead: false,
  body: { contentType: 'html', content: '<p>Original <b>full</b> body.</p><p>Second paragraph.</p>' }, bodyPreview: 'Incorrect preview is never used', has_attachments: false, web_link: 'https://outlook.office365.com/mail/id/original', ...changes });
const response = entries => ({ value: entries, has_more: false, next_from_index: null, next_link: null });
const project = (entries, options = selected, definition = source) => outlookPage(response(entries), profile, definition, options, observed);

test('inline HTML references require media coverage even when the provider attachment flag is false', () => {
  const plain = project([message()]).messages[0];
  const first = project([message({ body: { contentType: 'html', content: '<p>Same text</p><img src="cid:first">' } })]);
  const changed = project([message({ body: { contentType: 'html', content: '<p>Same text</p><img src="cid:second">' } })]);
  assert.equal(first.messagesWithAttachments, 1); assert.equal(first.messages[0].attachments, 'expected');
  assert.match(first.messages[0].nativeRevision, /^outlook-inline-content:/);
  assert.equal(first.messages[0].text, changed.messages[0].text);
  assert.notEqual(first.messages[0].nativeRevision, changed.messages[0].nativeRevision); assert.notEqual(first.messages[0].nativeRevision, plain.nativeRevision);
});

test('Outlook projection binds the actual profile interval body and stable content revision', () => {
  assert.deepEqual(microsoftProfile({ ...profile, displayName: 'Not identity' }), profile);
  const page = project([message()]); validateMicrosoft('OutlookPage', page);
  assert.equal(page.complete, true); assert.equal(page.nextIndex, null); assert.equal(page.messages.length, 1);
  const item = page.messages[0]; assert.equal(item.accountId, profile.id); assert.equal(item.senderId, 'sender@example.test');
  assert.equal(item.messageId, hashJson(['outlook-message', 'native-message'])); assert.equal(item.conversationId, item.messageId);
  assert.equal(item.text, 'Original full body.\n\nSecond paragraph.'); assert.equal(item.occurredAt, '2026-09-07T23:00:00.000Z');
  assert.equal(outlookPage(response([message()]), profile, source, selected, '2026-09-08T04:00:02.000Z').messages[0].nativeRevision, item.nativeRevision);
  assert.notEqual(project([message({ body: { contentType: 'text', content: 'Edited original body' } })]).messages[0].nativeRevision, item.nativeRevision);
  assert.notEqual(project([message({ has_attachments: true })]).messages[0].nativeRevision, item.nativeRevision);
  assert.equal(project([message({ has_attachments: true })]).messagesWithAttachments, 1);
  assert.equal(project([message({ has_attachments: true })]).messages[0].attachments, 'expected');
  assert.equal(item.attachments, 'none');
  const args = outlookArguments(selected); assert.equal(args.folder_id, 'inbox'); assert.equal(args.skip, 0); assert.equal(args.top, 8);
  assert.equal(args.filter, 'receivedDateTime ge 2026-09-01T00:00:00.000Z and receivedDateTime lt 2026-09-08T04:00:00.000Z and isRead eq false');
});

test('Outlook filters own and non-allowed senders before normalizing rejected message contents', () => {
  const own = message({ sender: { emailAddress: { address: 'ME@example.test' } }, body: 47, id: null, receivedDateTime: 'bad' });
  const unknown = message({ sender: { emailAddress: { address: 'unknown@example.test' } }, body: 47, id: null });
  const page = project([own, unknown, message()], selected, { ...source, ownSenderIds: [], allowedSenderIds: ['sender@example.test'] });
  assert.deepEqual(page.excluded, { ownSender: 1, senderNotAllowed: 1 }); assert.equal(page.messages.length, 1);
  assert.doesNotMatch(JSON.stringify(page), /unknown@example|ME@example|Incorrect preview/);
  assert.throws(() => project([message()], selected, { ...source, accountId: 'different' }), { code: 'microsoft_account_mismatch' });
  assert.throws(() => project([], selected, { ...source, ownSenderIds: ['ME@example.test'] }), { code: 'microsoft_sender_config_invalid' });
});

test('Outlook validates explicit increasing connector cursors and rejects incomplete or contradictory pages', () => {
  const value = { ...response([message()]), has_more: true, next_from_index: 13, next_link: 'https://graph.microsoft.com/v1.0/me/mailFolders/inbox/messages?$skip=13' };
  const page = outlookPage(value, profile, source, selected, observed); assert.equal(page.nextIndex, 13); assert.equal(page.complete, false);
  assert.equal(outlookArguments({ ...selected, skip: page.nextIndex }).skip, 13);
  for (const changes of [{ next_from_index: 0 }, { next_from_index: null }, { next_from_index: 1.5 }, { next_from_index: 1000001 }, { next_link: null }, { next_link: 'https://user:secret@graph.microsoft.com/' }, { next_link: 'https://other.test/' }, { value: [] }, { has_more: false }])
    assert.throws(() => outlookPage({ ...value, ...changes }, profile, source, selected, observed), { code: 'microsoft_page_incomplete' });
  assert.throws(() => project([message(), message()]), { code: 'microsoft_duplicate_id' });
  assert.throws(() => project([message(), message({ id: 'another' })], { ...selected, pageSize: 1 }), { code: 'microsoft_page_incomplete' });
  assert.throws(() => outlookPage({ value: [] }, profile, source, selected, observed));
});

test('Outlook refuses out-of-window read changed order invalid times or missing original bodies without truncation', () => {
  for (const changes of [{ receivedDateTime: selected.until }, { receivedDateTime: '2026-08-31T23:59:59Z' }, { isRead: true }])
    assert.throws(() => project([message(changes)]), { code: 'microsoft_selection_mismatch' });
  for (const time of ['2026-02-30T00:00:00Z', '2026-09-07T23:00:00.0000001Z', '2026-09-07T23:00:00+00:00']) assert.throws(() => project([message({ receivedDateTime: time })]), { code: 'microsoft_time_invalid' });
  assert.equal(project([message({ receivedDateTime: '2026-09-07T23:00:00.1230000Z' })]).messages[0].occurredAt, '2026-09-07T23:00:00.123Z');
  assert.throws(() => project([message({ receivedDateTime: '2026-09-07T22:00:00Z' }), message({ id: 'later' })]), { code: 'microsoft_selection_mismatch' });
  assert.throws(() => outlookArguments({ ...selected, until: selected.since }));
  assert.throws(() => outlookPage(response([]), profile, source, selected, selected.since));
  for (const body of [null, { contentType: 'text' }, { contentType: 'unknown', content: 'body' }, { contentType: 'text', content: 'x'.repeat(32769) }]) assert.throws(() => project([message({ body })]));
  assert.equal(project([message({ body: { contentType: 'text', content: '' } })]).messages[0].text, '');
  assert.throws(() => project([message({ subject: 'x'.repeat(1025) })]));
  assert.throws(() => project([message({ web_link: 'javascript:alert(1)' })]), { code: 'microsoft_url_invalid' });
  assert.throws(() => project(Array.from({ length: 8 }, (_, i) => message({ id: String(i), body: { contentType: 'text', content: 'x'.repeat(32768) } }))));
});

test('Outlook HTML preserves text block link and image-alt evidence without executable or preview fallback', () => {
  const body = outlookBody({ contentType: 'html', content: '<html><head><style>SECRET</style></head><body><p>One &amp; two</p><div>Next<br>line</div><table><tr><td>A</td><td>B</td></tr></table><p><a href="https://example.test/path">Source</a></p><img src="cid:private-image" alt="Invoice chart"><script>SECRET</script><template>SECRET</template><a href="javascript:alert(1)">Visible label</a></body></html>' });
  assert.match(body, /One & two/); assert.match(body, /Next\nline/); assert.match(body, /A B/); assert.match(body, /Source \(https:\/\/example.test\/path\)/); assert.match(body, /Invoice chart/); assert.match(body, /Visible label/);
  assert.doesNotMatch(body, /SECRET|javascript|cid:|<script>/);
  assert.throws(() => outlookBody({ contentType: 'text', content: '\u0000' }));
  assert.throws(() => outlookBody({ contentType: 'html', content: '<p>' + 'x'.repeat(32769) + '</p>' }));
  assert.throws(() => outlookBody({ contentType: 'html', content: '<a href="https://example.test/' + 'x'.repeat(4096) + '">link</a>' }));
});

test('Microsoft MCP retains rate-limit delays and rejects malformed provider success and retry metadata', () => {
  assert.deepEqual(microsoftMcp({ content: [], structuredContent: { value: [] }, isError: false }), { value: [] });
  assert.throws(() => microsoftMcp({ content: [], isError: true, structuredContent: { error_code: 'RATE_LIMITED', retry_after_seconds: 62 } }), error => error.code === 'microsoft_rate_limited' && error.details.retryAfterSeconds === 62 && error.outcome === 'completed');
  for (const retry_after_seconds of [0, -1, 1.5, 86401, '62', null]) assert.throws(() => microsoftMcp({ content: [], isError: true, structuredContent: { error_code: 'RATE_LIMITED', retry_after_seconds } }), { code: 'microsoft_retry_invalid' });
  assert.throws(() => microsoftMcp({ content: [], isError: true, structuredContent: { error_code: 'FORBIDDEN' } }), { code: 'microsoft_provider_failed' });
  assert.throws(() => microsoftMcp({ content: [], structuredContent: null })); assert.throws(() => microsoftMcp({ structuredContent: {} }));
  assert.throws(() => microsoftProfile({ id: profile.id, email: 'not-an-address' }));
});
