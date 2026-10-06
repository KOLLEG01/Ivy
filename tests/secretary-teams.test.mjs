import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, chmod } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { hashJson } from '../dist/packages/sdk/src/node.js';
import { teamsPage, teamsArguments } from '../dist/services/secretary/src/teams-projection.js';
import { teamsConfiguration, loadTeamsConfiguration } from '../dist/services/secretary/src/teams-config.js';
import { validateMicrosoft } from '../dist/services/secretary/src/microsoft-schema.js';
import { chat, message, profile } from './secretary-teams-fixture.mjs';

const config = { sourceId: 'teams', target: { serviceNodeId: 'native', hostId: 'host', nativeVersion: '0.149.1', nativeExecutableHash: hashJson('exe'), catalogHash: hashJson('catalogue') },
  expectedAccountHash: hashJson('account'), profile, since: '2026-09-01T00:00:00.000Z', maximumChats: 8, maximumMessages: 8, excludedChatIds: ['excluded'], pollMs: 60000, threadCwd: resolve('.local/teams-unit') };
const source = { sourceId: 'teams', kind: 'teams', accountId: profile.id, producerPrincipalIds: ['producer'], ownSenderIds: [], allowedSenderIds: null };
const selection = { since: config.since, until: '2026-09-08T04:00:00.000Z', chat: { id: 'chat-1', title: '' } }, observed = '2026-09-08T04:00:01.000Z';
const project = (messages, selected = selection, definition = source) => teamsPage({ messages }, config, definition, selected, observed);

test('Teams projects actual native chat and message shapes with exact content identities and no preview fallback', () => {
  const chats = teamsPage({ chats: [chat(), chat('excluded'), chat('hidden', { is_hidden: true })] }, config, source, { ...selection, chat: null }, observed);
  assert.deepEqual(chats.chats, [{ id: 'chat-1', title: '' }]); assert.equal(chats.excluded.configuredChat, 1); assert.equal(chats.excluded.hiddenChat, 1); assert.equal(chats.messages.length, 0);
  assert.doesNotMatch(JSON.stringify(chats), /Never used/); assert.deepEqual(teamsArguments(config, { ...selection, chat: null }), { unread_only: true, top: 9 });
  assert.deepEqual(teamsArguments(config, selection), { chat_id: 'chat-1', sent_after: config.since, top: 9 });
  const page = project([message()]), item = page.messages[0]; validateMicrosoft('TeamsPage', page);
  assert.equal(item.text, message().content); assert.equal(item.senderId, 'user:sender'); assert.equal(item.accountId, profile.id);
  assert.equal(item.conversationId, hashJson(['teams-chat', 'chat-1'])); assert.equal(item.messageId, hashJson(['teams-message', 'chat-1', 'original']));
  assert.equal(teamsPage({ messages: [message()] }, config, source, selection, '2026-09-08T04:00:02.000Z').messages[0].nativeRevision, item.nativeRevision);
  for (const changes of [{ content: 'Edited body' }, { has_attachments: true }, { mentions: [{ mention_id: 1, mentioned_user_id: 'another' }] }]) assert.notEqual(project([message('original', changes)]).messages[0].nativeRevision, item.nativeRevision);
  assert.equal(project([message('media', { has_attachments: true })]).messagesWithAttachments, 1);
  assert.equal(project([message('media', { has_attachments: true })]).messages[0].attachments, 'expected');
  assert.equal(item.attachments, 'expected');
  assert.equal(project([message()]).messagesWithAttachments, 0); // Native flag count is not media coverage.
  const oldRevision = 'teams-content:' + hashJson({ chatId: 'chat-1', id: 'original', senderId: item.senderId, occurredAt: item.occurredAt, title: item.title,
    text: item.text, hasAttachments: false, mentions: [] }).slice(7);
  assert.notEqual(item.nativeRevision, oldRevision); // Never reinterpret an earlier text-only identity.
  assert.equal(project([message('app', { author_user_id: null, author_application_id: 'bot' })]).messages[0].senderId, 'application:bot');
  const nativeLink = 'https://teams.microsoft.com/l/message/chat-1/original?context={"contextType":"chat"}';
  const normalized = project([message('original', { web_link: nativeLink })]).messages[0].url;
  assert.equal(normalized, new URL(nativeLink).href); assert.equal(new URL(normalized).searchParams.get('context'), '{"contextType":"chat"}');
  for (const invalid of ['https://user:secret@teams.microsoft.com/path', 'http://teams.microsoft.com/path', '/l/message/local'])
    assert.throws(() => project([message('original', { web_link: invalid })]), { code: 'teams_url_invalid' });
});

test('Teams excludes own disallowed deleted system and later messages before content admission', () => {
  const messages = [message('own', { author_user_id: profile.id, content: null }), message('unknown', { author_user_id: 'unknown', content: null }),
    message('deleted', { deleted_at: '2026-09-07T08:30:00.000Z', author_user_id: null, content: null }), message('system', { message_type: 'systemEventMessage', author_user_id: null, content: null }),
    message('future', { created_at: selection.until, content: null }), message()];
  const page = project(messages, selection, { ...source, allowedSenderIds: ['user:sender'] });
  assert.equal(page.messages.length, 1); assert.deepEqual(page.excluded, { hiddenChat: 0, configuredChat: 0, ownSender: 1, senderNotAllowed: 1, deleted: 1, system: 1, outsideWindow: 1 });
  assert.doesNotMatch(JSON.stringify(page.messages), /unknown|deleted|system|future/);
});

test('Teams rejects truncated duplicate cross-chat malformed time and incomplete body responses as whole pages', () => {
  assert.throws(() => project(Array.from({ length: 9 }, (_, i) => message(String(i)))), { code: 'teams_scan_limit' });
  assert.throws(() => teamsPage({ chats: Array.from({ length: 9 }, (_, i) => chat(String(i))) }, config, source, { ...selection, chat: null }, observed), { code: 'teams_scan_limit' });
  for (const changes of [{ chats: [chat(), chat()] }, { chats: [chat('read', { is_unread: false })] }, { chats: [chat()], has_more: true }])
    assert.throws(() => teamsPage(changes, config, source, { ...selection, chat: null }, observed));
  assert.throws(() => project([message(), message()]), { code: 'teams_duplicate_id' });
  for (const changes of [{ chat_id: 'elsewhere' }, { path: '/chats/elsewhere/messages/original' }, { container_type: 'channel' }, { parent_message_id: 'another' },
    { created_at: '2026-02-30T00:00:00Z' }, { created_at: '2026-09-07T08:00:00.1230001Z' }, { created_at: config.since.replace('09-01', '08-31') }, { content: null },
    { content: 'x'.repeat(32769) }, { title: 'x'.repeat(1025) }, { content: '\ud800' }, { message_type: null }, { author_user_id: null }, { author_application_id: 'ambiguous' },
    { web_link: 'javascript:alert(1)' }, { mentions: null }, { has_attachments: undefined }]) assert.throws(() => project([message('original', changes)]));
  assert.equal(project([message('empty', { content: '' })]).messages[0].text, '');
  assert.throws(() => project(Array.from({ length: 8 }, (_, i) => message(String(i), { content: 'x'.repeat(32768) }))));
  assert.throws(() => project([], selection, { ...source, accountId: 'another' }), { code: 'microsoft_account_mismatch' });
  assert.throws(() => project([], selection, { ...source, ownSenderIds: ['name@example.test'] }), { code: 'teams_sender_config_invalid' });
});

test('Teams configuration enforces protected bounded files and exact producer account selection', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-teams-config-')), settings = { identity: { principalId: 'producer' }, sources: [source] }, value = { schemaVersion: 1, collectors: [config] };
  const path = join(directory, 'config.json');assert.deepEqual(await loadTeamsConfiguration(path, settings), { schemaVersion: 1, collectors: [] });
  await writeFile(path, JSON.stringify({schemaVersion:1,teams:value}), { mode: 0o600 });
  await assert.rejects(loadTeamsConfiguration(path, settings), { code: 'teams_native_source_not_ready' });
  assert.deepEqual(teamsConfiguration({ schemaVersion: 1, collectors: [] }, settings), { schemaVersion: 1, collectors: [] });
  for (const changed of [{ sourceId: 'wrong' }, { profile: { ...profile, id: 'other' } }, { threadCwd: 'relative' }, { maximumChats: 0 }, { maximumMessages: 33 }, { excludedChatIds: ['same', 'same'] }, { since: '2026-02-30T00:00:00.000Z' }])
    assert.throws(() => teamsConfiguration({ schemaVersion: 1, collectors: [{ ...config, ...changed }] }, settings));
  assert.throws(() => teamsConfiguration({ schemaVersion: 1, collectors: [config, config] }, settings));
  await writeFile(path, 'x'.repeat(1024*1024+1)); await assert.rejects(loadTeamsConfiguration(path, settings), { code: 'teams_config_unprotected' });
  if (process.platform !== 'win32') { await writeFile(path, JSON.stringify({schemaVersion:1,teams:value})); await chmod(path, 0o644); await assert.rejects(loadTeamsConfiguration(path, settings), { code: 'teams_config_unprotected' }); }
});
