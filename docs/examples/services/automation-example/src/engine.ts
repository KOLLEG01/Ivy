import { bundledSchema, canonical, hashJson, IvyError, requireThat, scopedOperationId, validateAutomation } from '../../../../../packages/sdk/src/node.js';
import type { Automation, Operation, RpcClient, Wire } from '../../../../../packages/sdk/src/node.js';

export function automationRegistry(): Wire.RegistrySync {
  const entries = [['checkpoint', 'Checkpoint'], ['period', 'Period'], ['event-receipt', 'EventReceipt']] as const;
  const contracts = entries.map(([name, schema]) => ({ key: key(name), version: '1.0.0', owner: { kind: 'service' as const, serviceName: 'automation-example' },
    mediaType: 'application/json', retention: { objects: { mode: 'retain' as const }, revisions: { mode: 'all' as const } },
    jsonSchema: bundledSchema('#/$defs/' + schema, 'https://ivy.invalid/schemas/automation.schema.json'),
    specMarkdown: `Durable automation ${name} records owned by the SDK example.` }));
  return { discoveryHint: '', namespaces: [], contracts, requiredContracts: contracts.map(contract => ({ key: contract.key, readVersions: [contract.version], writeVersions: [contract.version] })) };
}

export function validDate(value: string): string {
  const parsed = new Date(value + 'T12:00:00Z');
  requireThat(/^\d{4}-\d{2}-\d{2}$/.test(value) && value >= '1970-01-01' && value <= '9998-12-31' &&
    Number.isFinite(parsed.getTime()) && parsed.toISOString().slice(0, 10) === value, 'invalid_arguments', 'Expected a real calendar date from 1970 through9998.');
  return value;
}
export function shiftDate(value: string, days: number): string {
  const date = new Date(validDate(value) + 'T12:00:00Z'); date.setUTCDate(date.getUTCDate() + days); return date.toISOString().slice(0, 10);
}
export function latestDueDate(definition: Automation.Definition, now: Date): string {
  const parts = new Intl.DateTimeFormat('en-CA', { timeZone: definition.timeZone, calendar: 'iso8601', numberingSystem: 'latn',
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).formatToParts(now);
  const part = (name: string) => parts.find(p => p.type === name)!.value;
  const today = validDate(part('year') + '-' + part('month') + '-' + part('day'));
  return part('hour') + ':' + part('minute') >= definition.localTime ? today : shiftDate(today, -1);
}

type Value = Automation.Checkpoint | Automation.Period | Automation.EventReceipt;
interface Document<T> { objectId: string; revision: number; value: T }
const key = (name: string) => 'automation-example/' + name;
const mutation = (request: unknown) => 'automation:' + hashJson(request).slice(7);

/** The only effect is a saved Hive Object. Every retry derives from a durable original claim. */
export class AutomationEngine {
  readonly definition: Automation.Definition;
  readonly definitionHash: string;
  readonly prefix: string;
  constructor(readonly client: RpcClient, readonly settings: Automation.Settings, readonly now: () => Date = () => new Date()) {
    validateAutomation('Settings', settings); this.definition = structuredClone(settings.definition);
    validDate(this.definition.firstDate);
    try { new Intl.DateTimeFormat('en', { timeZone: this.definition.timeZone }).format(); }
    catch { requireThat(false, 'invalid_arguments', 'Automation requires a supported IANA timezone.'); }
    this.definitionHash = hashJson(this.definition);
    this.prefix = 'automation-' + this.definition.scheduleId;
  }
  private async named<T extends Value>(name: string, contract: string, schema: string): Promise<Document<T> | null> {
    const parent = this.definition.rootObjectId;
    const page = await this.client.request('objects.query', { contractKey: key(contract), includeArchived: true, limit: 2,
      where: { op: 'and', args: [parent === null ? { op: 'isNull', field: 'object.parentId' } : { op: 'eq', field: 'object.parentId', value: parent },
        { op: 'eq', field: 'object.name', value: name }] } });
    requireThat(page.items.length <= 1 && !page.nextCursor, 'automation_identity_conflict', 'An automation name must identify one original record.');
    if (!page.items[0]) return null;
    const value = await this.client.request('objects.read', { objectId: page.items[0].objectId });
    requireThat(value.object.parentId === parent && value.object.name === name && value.object.contractKey === key(contract) &&
      !value.object.effectivelyArchived && value.revision.contractVersion === '1.0.0' && value.revision.mediaType === 'application/json' && value.content.encoding === 'json',
    'automation_contract_mismatch', 'Automation record scope, archive state or version changed.');
    requireThat(contract === 'checkpoint' || value.revision.revision === 1, 'automation_identity_conflict', 'A saved period or event receipt must retain its original immutable revision.');
    validateAutomation(schema, value.content.value);
    const content = value.content.value as T;
    requireThat(content.definitionHash === this.definitionHash && hashJson(content) === value.revision.contentHash &&
      Buffer.byteLength(canonical(content)) === value.revision.byteLength, 'automation_identity_conflict', 'Automation record identity or saved bytes changed.');
    return { objectId: value.object.id, revision: value.revision.revision, value: content };
  }
  private async create<T extends Value>(name: string, contract: string, schema: string, value: T): Promise<Document<T>> {
    validateAutomation(schema, value);
    const existing = await this.named<T>(name, contract, schema);
    if (existing) { requireThat(hashJson(existing.value) === hashJson(value), 'automation_identity_conflict', 'An immutable effect already has different saved content.'); return existing; }
    const request = { contractVersion: '1.0.0', references: {}, create: { parentId: this.definition.rootObjectId, ownerObjectId: null, name, contractKey: key(contract) }, content: { encoding: 'json' as const, value: value as Wire.Json } };
    try {
      const result = await this.client.request('objects.write', { ...request, mutationId: await scopedOperationId(this.client, mutation(request)) });
      return { objectId: result.object.id, revision: result.revision.revision, value };
    } catch (error) {
      if (!(error instanceof IvyError) || error.code !== 'revision_conflict') throw error;
      const winner = await this.named<T>(name, contract, schema);
      requireThat(winner && hashJson(winner.value) === hashJson(value), 'automation_identity_conflict', 'A competing immutable effect saved different content.');
      return winner;
    }
  }
  private check(value: Automation.Checkpoint): void {
    requireThat(hashJson(value.definition) === this.definitionHash && value.definitionHash === this.definitionHash && validDate(value.nextDate) >= this.definition.firstDate,
      'automation_identity_conflict', 'This schedule identity belongs to another definition or invalid checkpoint.');
    const pending = value.pending;
    if (!pending) return;
    validDate(pending.periodDate); validDate(pending.advanceToDate);
    requireThat(pending.periodDate >= value.nextDate && pending.advanceToDate === shiftDate(pending.periodDate, 1) &&
      (this.definition.catchUp === 'latest' || pending.periodDate === value.nextDate), 'automation_state_conflict', 'Pending period cannot move scheduling backwards or skip an all-period schedule.');
    const skipped = pending.periodDate > value.nextDate;
    requireThat(pending.skippedFromDate === (skipped ? value.nextDate : null) && pending.skippedThroughDate === (skipped ? shiftDate(pending.periodDate, -1) : null),
      'automation_state_conflict', 'The retained skipped range must be exact.');
  }
  private async checkpoint(): Promise<Document<Automation.Checkpoint>> {
    const value = await this.named<Automation.Checkpoint>(this.prefix + '-checkpoint', 'checkpoint', 'Checkpoint');
    requireThat(value, 'automation_checkpoint_missing', 'The original checkpoint must exist before work.'); this.check(value.value); return value;
  }
  private async advance(previous: Document<Automation.Checkpoint>, value: Automation.Checkpoint): Promise<void> {
    validateAutomation('Checkpoint', value); this.check(value);
    const request = { objectId: previous.objectId, expectedRevision: previous.revision, contractVersion: '1.0.0', references: {}, content: { encoding: 'json' as const, value: value as Wire.Json } };
    await this.client.request('objects.write', { ...request, mutationId: await scopedOperationId(this.client, mutation(request)) });
  }
  async initialize(owner: { serviceNodeId: string; generation: number }): Promise<void> {
    const observed = await this.client.request('serviceNodes.get', { serviceNodeId: owner.serviceNodeId });
    requireThat(observed.principalId === this.definition.principalId && observed.connected,
      'automation_owner_mismatch', 'The live SDK connection must authenticate as the original logical automation principal.');
    if (this.definition.rootObjectId) { const root = await this.client.request('objects.stat', { objectId: this.definition.rootObjectId }); requireThat(!root.effectivelyArchived, 'automation_root_archived', 'The configured output root is archived.'); }
    const retained = await this.named<Automation.Checkpoint>(this.prefix + '-checkpoint', 'checkpoint', 'Checkpoint');
    if (retained) this.check(retained.value);
    else await this.create(this.prefix + '-checkpoint', 'checkpoint', 'Checkpoint', { schemaVersion: 1, definition: this.definition,
      definitionHash: this.definitionHash, nextDate: this.definition.firstDate, pending: null });
  }
  async tick(): Promise<number> {
    let completed = 0;
    // Three bounded claim/publication stages plus one completion for each selected period.
    for (let transitions = 0; transitions < this.settings.maximumPeriodsPerTick * 4; transitions++) {
      const record = await this.checkpoint(), checkpoint = record.value, pending = checkpoint.pending;
      if (!pending) {
        const due = latestDueDate(this.definition, this.now());
        if (checkpoint.nextDate > due || completed >= this.settings.maximumPeriodsPerTick) return completed;
        const periodDate = this.definition.catchUp === 'all' ? checkpoint.nextDate : due, skipped = periodDate > checkpoint.nextDate;
        await this.advance(record, { ...checkpoint, pending: { periodDate, advanceToDate: shiftDate(periodDate, 1),
          skippedFromDate: skipped ? checkpoint.nextDate : null, skippedThroughDate: skipped ? shiftDate(periodDate, -1) : null, snapshot: null } });
      } else if (!pending.snapshot) {
        const snapshot = await this.client.request('system.status', {});
        await this.advance(record, { ...checkpoint, pending: { ...pending, snapshot } });
      } else {
        const period: Automation.Period = { schemaVersion: 1, definitionHash: this.definitionHash, periodDate: pending.periodDate,
          timeZone: this.definition.timeZone, localTime: this.definition.localTime, skippedFromDate: pending.skippedFromDate,
          skippedThroughDate: pending.skippedThroughDate, snapshot: pending.snapshot };
        await this.create(this.prefix + '-period-' + pending.periodDate, 'period', 'Period', period);
        await this.advance(record, { ...checkpoint, nextDate: pending.advanceToDate, pending: null }); completed++;
      }
    }
    return completed;
  }
  async persistEvents(batch: Operation.EventBatch, signal: AbortSignal): Promise<void> {
    requireThat(batch.gap === null, 'event_gap', 'Automation cannot acknowledge pruned input without an application snapshot.');
    for (const event of batch.items) {
      signal.throwIfAborted();
      requireThat(event.source === this.definition.inputSource && event.topic === this.definition.inputTopic,
        'automation_event_mismatch', 'An event outside the exact configured source/topic cannot be acknowledged.');
      const receipt: Automation.EventReceipt = { schemaVersion: 1, definitionHash: this.definitionHash, sequence: event.sequence,
        topic: event.topic, topicVersion: event.topicVersion, source: event.source, occurredAt: event.occurredAt,
        sourceMutationId: event.mutationId, payloadHash: hashJson(event.payload) };
      await this.create(this.prefix + '-event-' + event.sequence, 'event-receipt', 'EventReceipt', receipt);
    }
  }
}
