import assert from 'node:assert/strict';
import { discover, callBound } from '../../../dist/packages/sdk/src/client.js';

// Preserve available native usage snapshots without estimating tokens or summing
// cumulative thread totals. Missing/compacted observations stay explicitly partial.
export async function collectTokenEvidence(client, owner, epoch, turnIds) {
  const binding = await discover(client, 'agent.notifications', { serviceNodeId: owner });
  const selected = new Set(turnIds), latest = new Map();
  let afterSequence = 0, gap = false, hasMore = true, pages = 0;
  while (hasMore && pages < 100) {
    const page = await callBound(client, binding, { afterSequence, limit: 100 }, undefined, { timeoutMs: 30000 });
    assert.equal(page.epoch, epoch); assert.ok(page.throughSequence >= afterSequence);
    gap ||= page.gap;
    for (const item of page.items) {
      if (item.method !== 'thread/tokenUsage/updated' || !selected.has(item.params.turnId)) continue;
      latest.set(item.params.turnId, { turnId: item.params.turnId, threadId: item.params.threadId, sequence: item.sequence,
        observedAt: item.observedAt, tokenUsage: item.params.tokenUsage });
    }
    hasMore = page.hasMore;
    assert.ok(!hasMore || page.throughSequence > afterSequence, 'Native notification cursor must advance');
    afterSequence = page.throughSequence; pages++;
  }
  return { state: gap || hasMore || latest.size > 0 && latest.size < selected.size ? 'partial' : latest.size ? 'available' : 'not_observed',
    gap, boundedScanIncomplete: hasMore, pages, throughSequence: afterSequence,
    missingTurnIds: [...selected].filter(id => !latest.has(id)), snapshots: [...latest.values()] };
}
