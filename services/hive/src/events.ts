import { canonical } from '../../../packages/contracts/src/canonical.js';
import type { Json } from '../../../packages/contracts/src/canonical.js';
import { requireThat } from '../../../packages/contracts/src/errors.js';
import type { EventBatch, EventFilter, HiveEvent, Schema } from '../../../packages/contracts/src/types.js';
import { HiveStore } from './store.js';
import type { ServiceContext } from './store.js';

export const maximumSubscriptionsPerNode = 64;

export class Events {
  constructor(readonly store: HiveStore) {}
  registerTopic(definition: { topic: string; version: string; payloadSchema: Schema }, owner: string): void {
    this.store.validators.compile(definition.payloadSchema);
    const encoded = canonical({ topic: definition.topic, version: definition.version, payloadSchema: definition.payloadSchema });
    const prior = this.store.get('SELECT * FROM topics WHERE topic=? AND version=?', definition.topic, definition.version);
    requireThat(!prior || (prior['owner'] === owner && prior['definition'] === encoded), 'registry_invalid', 'Topic definitions are immutable and owner-bound.');
    this.store.run('INSERT OR IGNORE INTO topics VALUES (?,?,?,?)', definition.topic, definition.version, owner, encoded);
  }
  publish(context: ServiceContext, params: { topic: string; topicVersion: string; payload: Json; mutationId: string }): { sequence: number; topic: string; publishedAt: string } {
    const node = this.store.get('SELECT service_name FROM service_nodes WHERE id=?', context.serviceNodeId);
    requireThat(node, 'not_found', 'Service Node not found.');
    return this.store.mutate(context, 'events.publish', params, () => {
      const row = this.store.get('SELECT * FROM topics WHERE topic=? AND version=?', params.topic, params.topicVersion);
      requireThat(row && row['owner'] === 'service:' + String(node['service_name']), 'registry_invalid', 'The service does not own this topic version.');
      const registryRow = this.store.get('SELECT registry_json FROM registries WHERE node_id=?', context.serviceNodeId);
      const registry = registryRow ? JSON.parse(String(registryRow['registry_json'])) as { namespaces: { topics: { topic: string; version: string }[] }[] } : null;
      requireThat(registry?.namespaces.some(ns => ns.topics.some(topic => topic.topic === params.topic && topic.version === params.topicVersion)), 'registry_invalid', 'Topic is not in this node\'s current registry.');
      const definition = JSON.parse(String(row['definition'])) as { payloadSchema: Schema };
      this.store.validators.validate(definition.payloadSchema, params.payload);
      const now = new Date().toISOString();
      const payload = canonical(params.payload, 4096);
      this.store.admitEvent();
      const result = this.store.run('INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES (?,?,?,?,?,?)', params.topic, params.topicVersion, 'service:' + context.serviceNodeId, now, params.mutationId, payload);
      const sequence = Number(result.lastInsertRowid);
      const value = { sequence, topic: params.topic, publishedAt: now };
      return { value };
    });
  }
  head(): { throughSequence: number } {
    const pruned = Number(this.store.metadata('events_pruned_through') ?? 0),
      retained = Number(this.store.get('SELECT MAX(sequence) AS sequence FROM events')?.['sequence'] ?? 0);
    return { throughSequence: Math.max(pruned, retained) };
  }
  read(params: { afterSequence: number; filter: EventFilter; limit?: number }): EventBatch {
    const prunedThroughSequence = Number(this.store.metadata('events_pruned_through') ?? 0);
    if (params.afterSequence < prunedThroughSequence) return { items: [], throughSequence: prunedThroughSequence,
      hasMore: Boolean(this.store.get('SELECT 1 FROM events WHERE sequence>? LIMIT 1', prunedThroughSequence)),
      gap: { prunedThroughSequence, resumeAfterSequence: prunedThroughSequence } };
    const rows = this.store.all('SELECT * FROM events WHERE sequence>? ORDER BY sequence LIMIT 100', params.afterSequence);
    const items: HiveEvent[] = [];
    let throughSequence = params.afterSequence, bytes = 128;
    for (const row of rows) {
      this.store.checkBudget();
      const event = this.store.eventFromRow(row);
      const matches = (!params.filter.topics || params.filter.topics.includes(event.topic)) && (!params.filter.sources || params.filter.sources.includes(event.source)) &&
        (!params.filter.objectIds || (event.topic === 'hive.object.changed' && typeof (event.payload as Record<string, unknown>)['objectId'] === 'string' &&
          params.filter.objectIds.includes(String((event.payload as Record<string, unknown>)['objectId']))));
      if (matches) {
        const size = Buffer.byteLength(canonical(event));
        if (bytes + size > 1024 * 1024 - 1024) {
          requireThat(items.length > 0, 'result_too_large', 'Journal event cannot fit a deliverable batch.');
          break;
        }
        bytes += size; items.push(event);
      }
      throughSequence = event.sequence;
      if (items.length >= (params.limit ?? 100)) break;
    }
    const hasMore = Boolean(this.store.get('SELECT 1 FROM events WHERE sequence>? LIMIT 1', throughSequence));
    return { items, throughSequence, hasMore, gap: null };
  }
  subscribe(context: ServiceContext, params: { name: string; filter: EventFilter; initialSequence?: number; durable?: boolean; limit?: number }): EventBatch {
    return this.store.transaction(() => {
      if (params.durable) {
        const node = this.store.get('SELECT service_name FROM service_nodes WHERE id=?', context.serviceNodeId);
        requireThat(node?.['service_name'] === 'secretary', 'forbidden', 'Durable event subscriptions belong to Secretary.');
      }
      const filter = canonical(params.filter);
      let subscription = this.store.get('SELECT * FROM subscriptions WHERE node_id=? AND name=?', context.serviceNodeId, params.name);
      if (!subscription) {
        requireThat(Number(this.store.get('SELECT COUNT(*) AS count FROM subscriptions WHERE node_id=?', context.serviceNodeId)!['count']) < maximumSubscriptionsPerNode,
          'capacity_exceeded', 'Service Node subscription limit reached.');
        const initial = params.initialSequence ?? 0;
        this.store.run('INSERT INTO subscriptions(node_id,name,filter_json,initial_sequence,acknowledged_sequence,batch_json) VALUES (?,?,?,?,?,NULL)',
          context.serviceNodeId, params.name, filter, initial, initial);
        subscription = this.store.get('SELECT * FROM subscriptions WHERE node_id=? AND name=?', context.serviceNodeId, params.name)!;
      }
      requireThat(subscription['filter_json'] === filter && (params.initialSequence === undefined || params.initialSequence === subscription['initial_sequence']), 'subscription_filter_conflict', 'Subscription filter and initial cursor are immutable.');
      if (params.durable) this.store.run('INSERT OR IGNORE INTO durable_subscriptions(node_id,name) VALUES (?,?)', context.serviceNodeId, params.name);
      if (subscription['batch_json']) {
        const issued = JSON.parse(String(subscription['batch_json'])) as { sequences: number[]; throughSequence: number; hasMore: boolean; gap?: EventBatch['gap'] };
        if (issued.gap) return { items: [], throughSequence: issued.throughSequence, hasMore: issued.hasMore, gap: issued.gap };
        const rows = issued.sequences.map(sequence => this.store.get('SELECT * FROM events WHERE sequence=?', sequence));
        if (rows.some(row => !row)) this.store.run('UPDATE subscriptions SET batch_json=NULL WHERE node_id=? AND name=?', context.serviceNodeId, params.name);
        else return { items: rows.map(row => this.store.eventFromRow(row!)), throughSequence: issued.throughSequence, hasMore: issued.hasMore, gap: null };
      }
      const batch = this.read({ afterSequence: Number(subscription['acknowledged_sequence']), filter: params.filter, ...(params.limit === undefined ? {} : { limit: params.limit }) });
      const reference = { sequences: batch.items.map(event => event.sequence), throughSequence: batch.throughSequence, hasMore: batch.hasMore, ...(batch.gap ? { gap: batch.gap } : {}) };
      this.store.run('UPDATE subscriptions SET batch_json=? WHERE node_id=? AND name=?', canonical(reference), context.serviceNodeId, params.name);
      return batch;
    });
  }
  ack(context: ServiceContext, params: { name: string; throughSequence: number; gapThroughSequence?: number }): { acknowledgedSequence: number } {
    return this.store.transaction(() => {
      const subscription = this.store.get('SELECT * FROM subscriptions WHERE node_id=? AND name=?', context.serviceNodeId, params.name);
      requireThat(subscription, 'not_found', 'Subscription not found.');
      const issued = subscription['batch_json'] ? JSON.parse(String(subscription['batch_json'])) as { throughSequence: number; gap?: { prunedThroughSequence: number } } : null;
      // An empty batch still owns the outstanding slot. Release it before accepting another poll.
      if (subscription['acknowledged_sequence'] === params.throughSequence && issued?.throughSequence !== params.throughSequence) return { acknowledgedSequence: params.throughSequence };
      requireThat(issued?.throughSequence === params.throughSequence, 'invalid_ack', 'Acknowledgement must equal the outstanding delivered boundary.');
      requireThat(issued.gap ? params.gapThroughSequence === issued.gap.prunedThroughSequence : params.gapThroughSequence === undefined,
        'invalid_ack', 'A reported journal gap must be acknowledged explicitly.');
      this.store.run('UPDATE subscriptions SET acknowledged_sequence=?,batch_json=NULL WHERE node_id=? AND name=?', params.throughSequence, context.serviceNodeId, params.name);
      this.store.pruneEventJournalForCapacity();
      return { acknowledgedSequence: params.throughSequence };
    });
  }
  unsubscribe(context: ServiceContext, params: { name: string; mutationId: string }): { removed: boolean } {
    return this.store.mutate(context, 'events.unsubscribe', params, () => {
      this.store.run('DELETE FROM durable_subscriptions WHERE node_id=? AND name=?', context.serviceNodeId, params.name);
      const removed = this.store.run('DELETE FROM subscriptions WHERE node_id=? AND name=?', context.serviceNodeId, params.name).changes > 0;
      if (removed) this.store.pruneEventJournalForCapacity();
      return { value: { removed } };
    });
  }
}
