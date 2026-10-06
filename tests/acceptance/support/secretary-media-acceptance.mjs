import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { canonical, digest, hashJson } from '../../../dist/packages/sdk/src/node.js';
import { MediaSpool } from '../../../dist/services/secretary/src/media-spool.js';
import { OutlookMediaSource } from '../../../dist/services/secretary/src/outlook-media-source.js';
import { outlookTriageMedia } from '../../../dist/services/secretary/src/triage-media.js';
import { SecretaryTriage } from '../../../dist/services/secretary/src/triage.js';
import { TriageStore, triageName } from '../../../dist/services/secretary/src/triage-store.js';
import { loadVideoConfiguration, VideoPreparer } from '../../../dist/services/secretary/src/media-video.js';
import { prepareAcquiredMediaInput } from '../../../dist/services/secretary/src/media-document.js';

// The normal service owns collection, acquisition and model work. This helper only observes it.
// Recovery runs against an offline RPC facade and reads the original protected spool after restart.
export async function acceptMediaAssessment({ engine, source, directory, dataRoot, triage, recoveryOnly }) {
  const selectionPath = join(directory, 'media-item.json'); let item;
  try { item = JSON.parse(await readFile(selectionPath, 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; assert.ok(!recoveryOnly, 'Recovery needs the original media selection.'); }
  const deadline = Date.now() + 360000, store = new TriageStore(engine);
  let work;
  while (true) {
    if (!item) {
      let cursor;
      do {
        const page = await engine.client.request('objects.query', { contractKey: 'secretary/item', limit: 100, ...(cursor ? { cursor } : {}),
          orderBy: [{ field: 'object.id', direction: 'asc' }], where: { op: 'and', args: [
            { op: 'eq', field: 'object.parentId', value: engine.store.root },
            { op: 'eq', field: 'data:/sourceId', value: 'outlook' },
            { op: 'eq', field: 'data:/message/attachments', value: 'expected' } ] } });
        if (page.items.length) {
          item = { objectId: page.items[0].objectId, revision: 1 };
          await writeFile(selectionPath, JSON.stringify(item) + '\n', { flag: 'wx', mode: 0o600 }); break;
        }
        cursor = page.nextCursor;
      } while (cursor);
      assert.ok(item, 'No admitted unread Outlook item with attachments exists in this captured period. Do not mark an old message unread or replace the source.');
    }
    work = await store.find('secretary/triage-work', triageName('work', item));
    if (work?.value.phase === 'assessed') break;
    assert.ok(!work || work.value.phase === 'pending', 'Original media work ended with ' + work?.value.errorCode);
    assert.ok(!recoveryOnly && Date.now() < deadline, 'Original media acquisition/triage remains pending; resume the same state directory.');
    await new Promise(resolve => setTimeout(resolve, 1000));
  }
  const plan = await store.read('secretary/triage-plan', work.value.plan, undefined, true);
  assert.deepEqual(plan.value.settings, triage); assert.deepEqual(plan.value.input.item, item);
  const originalItem = await engine.item(item), assessed = await engine.item(work.value.assessment);
  assert.equal(originalItem.value.message.attachments, 'expected'); assert.equal(originalItem.value.decision, null);
  assert.equal(plan.value.input.contentScope, 'verified_media'); const media = plan.value.input.media;
  assert.ok(media && media.attachments.length > 0); assert.equal(media.gap, null);
  for (const attachment of media.attachments) {
    assert.notEqual(attachment.kind, 'unavailable'); assert.equal(attachment.errorCode, null);
    assert.ok(attachment.manifest && attachment.contentHash && attachment.byteLength > 0 && attachment.inputIndex >= 2);
  }
  const completion = await store.read('secretary/triage-completion', work.value.completion, undefined, true);
  assert.deepEqual(completion.value.plan, plan.pin); assert.deepEqual(assessed.value.decision.assessment, completion.value.result);
  assert.equal(assessed.value.decision.actor, engine.settings.identity.principalId);
  const calls = await engine.client.request('objects.query', { contractKey: 'secretary/triage-call', limit: 100, where: { op: 'and', args: [
    { op: 'eq', field: 'object.parentId', value: engine.store.root }, { op: 'eq', field: 'data:/plan/objectId', value: plan.pin.objectId } ] } });
  assert.equal(calls.nextCursor, null); const nativeCalls = [];
  for (const row of calls.items) {
    const call = await store.read('secretary/triage-call', { objectId: row.objectId, revision: row.revision });
    assert.deepEqual(call.value.plan, plan.pin); assert.ok(call.value.observation);
    const observation = await store.nativeJson('operation', [plan.pin, call.value.slot], call.value.observation);
    assert.equal(observation.phase, 'succeeded'); assert.equal(observation.operationId, call.value.operationId);
    nativeCalls.push({ slot: call.value.slot, method: call.value.method, operationId: call.value.operationId, observationHash: digest(canonical(observation, 8 * 1024 * 1024)) });
  }
  for (const method of ['thread/start', 'turn/start', 'thread/unsubscribe']) assert.equal(nativeCalls.filter(call => call.method === method).length, 1);
  assert.ok(nativeCalls.every(call => ['thread/start', 'thread/resume', 'turn/start', 'thread/unsubscribe'].includes(call.method)));
  nativeCalls.sort((a, b) => a.slot.localeCompare(b.slot));
  if (recoveryOnly) {
    const spool = await MediaSpool.open(join(dataRoot, 'outlook-media'), engine.settings.identity); let video;
    try {
      const configuration = await loadVideoConfiguration(join(directory,'config.json'));
      video = configuration ? new VideoPreparer(join(dataRoot, 'video-work'), configuration) : null;
      const mediaSource = new OutlookMediaSource(source, spool, async () => { throw Error('Recovery must never download an attachment.'); },
        (original, bytes, budget) => prepareAcquiredMediaInput(original, bytes, budget, video));
      const worker = new SecretaryTriage(engine, triage, async (sourceId, selected) => {
        assert.equal(sourceId, 'outlook'); const context = await mediaSource.context(selected); assert.ok(context); return outlookTriageMedia(context);
      });
      assert.equal(await worker.step(item), 'assessed');
      assert.deepEqual((await worker.native.work(item)).pin, work.pin);
    } finally { await video?.close(); spool.close(); }
  }
  const parameters = await engine.client.request('objects.read', plan.value.nativeParams);
  const result = { item, work: work.pin, plan: plan.pin, completion: completion.pin, assessment: work.value.assessment,
    mediaHash: hashJson(media), attachmentKinds: media.attachments.map(value => value.kind),
    parameterHash: parameters.revision.contentHash, assessmentHash: hashJson(completion.value.result), nativeCalls };
  const baselinePath = join(directory, 'media-baseline.json');
  try { assert.deepEqual(JSON.parse(await readFile(baselinePath, 'utf8')), result); }
  catch (error) { if (error.code !== 'ENOENT') throw error; assert.ok(!recoveryOnly); await writeFile(baselinePath, JSON.stringify(result) + '\n', { flag: 'wx', mode: 0o600 }); }
  return result;
}
