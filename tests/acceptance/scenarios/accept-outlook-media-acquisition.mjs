import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile, open } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { parseArgs } from 'node:util';
import { HiveClient, NativeOwner, callBound, discover, digest, hashJson } from '../../../dist/packages/sdk/src/node.js';
import { atomicJson } from '../../../dist/packages/sdk/src/host.js';
const accountIdentityHash = value => {
  const account = value?.account;
  assert.ok(account?.type === 'chatgpt' && typeof account.email === 'string' && account.email.length > 0);
  return hashJson({ type: account.type, email: account.email });
};
import { microsoftMcp, microsoftProfile } from '../../../dist/services/secretary/src/outlook-projection.js';
import { outlookAttachments, outlookMaterialization, downloadOutlookMedia } from '../../../dist/services/secretary/src/outlook-media.js';
import { validateMedia } from '../../../dist/services/secretary/src/media-schema.js';

// Actual read-only selected attachment acquisition. This gate does not claim unread-source
// admission, Hive media publication, installed Secretary supervision or message assessment.
async function main() {
const { values } = parseArgs({ options: Object.fromEntries(['host-config', 'binding-file', 'metadata-file', 'attachment-index', 'state-dir'].map(name => [name, { type: 'string' }])) });
for (const name of ['host-config', 'binding-file', 'metadata-file', 'attachment-index', 'state-dir']) assert.ok(values[name], 'Missing --' + name);
const directory = resolve(values['state-dir']), host = JSON.parse(await readFile(resolve(values['host-config']), 'utf8'));
assert.ok(host.hostId.endsWith('-acceptance'));
const identity = JSON.parse(await readFile(resolve(values['binding-file']), 'utf8'));
const originalMetadata = JSON.parse(await readFile(resolve(values['metadata-file']), 'utf8'));
const metadata = microsoftMcp(originalMetadata), index = Number(values['attachment-index']);
assert.ok(Number.isSafeInteger(index) && index >= 0);
const attachment = outlookAttachments(metadata, metadata.message_id)[index]; assert.ok(attachment);
assert.equal(attachment.attachmentType, 'fileAttachment'); assert.equal(attachment.payloadFetchSupported, true);
const agent = host.instances.find(instance => instance.instanceId === 'agent' && instance.componentId === 'agent-manager'); assert.ok(agent?.credential);
const client = new HiveClient(host.publicBaseUrl, { credential: agent.credential });
await mkdir(join(directory, 'project'), { recursive: true, mode: 0o700 });
const read = async name => { try { return JSON.parse(await readFile(join(directory, name + '.json'), 'utf8')); } catch (error) { if (error.code !== 'ENOENT') throw error; return null; } };
let seed = await read('seed');
if (!seed) {
  const status = await callBound(client, await discover(client, 'agent.status', { serviceNodeId: agent.serviceNodeId }), {});
  assert.equal(status.state, 'ready');
  const node = await client.request('serviceNodes.get', { serviceNodeId: agent.serviceNodeId });
  seed = { id: randomUUID(), hostId: host.hostId, endpoint: host.publicBaseUrl, caller: node.principalId, epoch: status.epoch,
    identityHash: hashJson(identity), metadataHash: hashJson(originalMetadata), attachment, startedAt: new Date().toISOString(),
    target: Object.fromEntries(['serviceNodeId', 'hostId', 'nativeVersion', 'nativeExecutableHash', 'catalogHash'].map(key => [key, status[key]])) };
  await writeFile(join(directory, 'seed.json'), JSON.stringify(seed) + '\n', { flag: 'wx', mode: 0o600 });
}
assert.equal(seed.identityHash, hashJson(identity)); assert.equal(seed.metadataHash, hashJson(originalMetadata)); assert.deepEqual(seed.attachment, attachment);
assert.equal(seed.hostId, host.hostId); assert.equal(seed.endpoint, host.publicBaseUrl); assert.equal(seed.target.serviceNodeId, agent.serviceNodeId);
const owner = new NativeOwner(client, seed.caller, seed.target);
const call = async (slot, method, params) => {
  let intent = await read(slot + '-intent');
  if (!intent) { const binding = await owner.binding(method); intent = { method, params, operationId: 'outlook-acquire-' + seed.id + '-' + slot, definitionHash: binding.definitionHash };
    await atomicJson(join(directory, slot + '-intent.json'), intent); }
  assert.equal(intent.method, method); assert.deepEqual(intent.params, params);
  const known = await read(slot + '-operation');
  if (known?.phase === 'succeeded') { owner.checkOperation(known, intent); return known.reply.result; }
  let operation = await owner.operation(intent);
  if ('kind' in operation) {
    assert.equal(known, null, 'An observed original call must never become new work.'); assert.equal(operation.epoch, seed.epoch);
    await owner.dispatch(intent, async () => assert.equal((await owner.status()).epoch, seed.epoch)); operation = await owner.operation(intent);
  }
  assert.ok(!('kind' in operation), 'The original native call remains unresolved.'); owner.checkOperation(operation, intent);
  await atomicJson(join(directory, slot + '-operation.json'), operation);
  assert.equal(operation.phase, 'succeeded', 'Inspect the original pending or failed operation; no substitute is dispatched.');
  assert.equal(operation.epoch, seed.epoch); return operation.reply.result;
};
const report = { schemaVersion: 1, id: seed.id, startedAt: seed.startedAt, invocationStartedAt: new Date().toISOString(), phase: 'starting',
  build: JSON.parse(await readFile(new URL('../../../dist/build-info.json', import.meta.url), 'utf8')), target: seed.target,
  scope: 'Actual account-bound selected file download and durable local recovery; no unread capture or Hive publication claim.',
  nativeModel: false, externalWrites: false, browserChanges: false, credentialCopied: false };
let threadId;
try {
  assert.equal(accountIdentityHash(await call('before', 'account/read', { refreshToken: false })), identity.expectedAccountHash);
  threadId = (await call('thread', 'thread/start', { cwd: join(directory, 'project'), ephemeral: true, permissions: ':danger-full-access', approvalPolicy: 'never' })).thread.id;
  const mcp = async (slot, tool, args = {}) => microsoftMcp(await call(slot, 'mcpServer/tool/call', { threadId, server: 'codex_apps', tool: 'microsoft_outlook_email.' + tool, arguments: args }));
  assert.deepEqual(microsoftProfile(await mcp('profile-before', 'get_profile')), identity.profile);
  const materialization = outlookMaterialization(await mcp('fetch', 'fetch_attachment', { message_id: attachment.messageId, attachment_id: attachment.attachmentId }), attachment);
  await atomicJson(join(directory, 'materialization.json'), materialization);
  let download = await read('download');
  if (download?.phase === 'downloading') {
    // A complete receipt and bytes may have committed before the final local state write.
    const evidence = await read('payload-evidence');
    if (evidence) {
      validateMedia('MediaDownloadEvidence', evidence); const bytes = await readFile(join(directory, 'payload.bin'));
      assert.equal(evidence.materializationHash, hashJson(materialization)); assert.equal(evidence.contentHash, digest(bytes)); assert.equal(evidence.byteLength, bytes.length);
      download = { phase: 'acquired', evidenceHash: hashJson(evidence), contentHash: evidence.contentHash, byteLength: bytes.length };
      await atomicJson(join(directory, 'download.json'), download);
    }
  }
  if (!download) {
    await atomicJson(join(directory, 'download.json'), { phase: 'downloading', materializationHash: hashJson(materialization), startedAt: new Date().toISOString() });
    // Immediately fetch the fresh URL and flush the complete bytes before any other native work.
    const { bytes, evidence } = await downloadOutlookMedia(materialization, new AbortController().signal);
    const payload = await open(join(directory, 'payload.bin'), 'wx', 0o600);
    try { await payload.writeFile(bytes); await payload.sync(); } finally { await payload.close(); }
    await atomicJson(join(directory, 'payload-evidence.json'), evidence);
    download = { phase: 'acquired', evidenceHash: hashJson(evidence), contentHash: evidence.contentHash, byteLength: bytes.length };
    await atomicJson(join(directory, 'download.json'), download);
  }
  assert.equal(download.phase, 'acquired', 'An interrupted download needs a new explicitly selected materialization scope; this original is never downloaded twice.');
  const evidence = await read('payload-evidence'), bytes = await readFile(join(directory, 'payload.bin'));
  validateMedia('MediaDownloadEvidence', evidence); assert.equal(evidence.materializationHash, hashJson(materialization));
  assert.equal(download.evidenceHash, hashJson(evidence)); assert.equal(evidence.contentHash, digest(bytes)); assert.equal(evidence.byteLength, bytes.length);
  assert.deepEqual(microsoftProfile(await mcp('profile-after', 'get_profile')), identity.profile);
  assert.equal(accountIdentityHash(await call('after', 'account/read', { refreshToken: false })), identity.expectedAccountHash);
  report.phase = 'passed'; report.evidence = evidence; report.originalMetadataHash = seed.metadataHash;
} catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
finally {
  if (threadId) { try { report.cleanup = await call('cleanup', 'thread/unsubscribe', { threadId }); assert.ok(['unsubscribed', 'notLoaded', 'notSubscribed'].includes(report.cleanup.status)); }
    catch (error) { report.cleanup = { code: error.code ?? error.name }; report.phase = 'failed'; process.exitCode = 1; } }
  report.finishedAt = new Date().toISOString();
  const path = join(directory, 'report-' + randomUUID() + '.json'); await atomicJson(path, report);
  console.log(JSON.stringify({ phase: report.phase, path, byteLength: report.evidence?.byteLength, contentHash: report.evidence?.contentHash, failure: report.failure ?? null }));
}
}
await main().catch(error => { console.error(JSON.stringify({ phase: 'preflight_failed', code: error.code ?? error.name, message: error.message })); process.exitCode = 1; });
