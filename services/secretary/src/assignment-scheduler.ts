import { hashJson, newOperationId, scopedOperationId, serviceTools } from '../../../packages/sdk/src/node.js';
import type { Agent, Operation, Wire } from '../../../packages/sdk/src/node.js';
import type { SecretaryEngine } from './engine.js';
import type { Assignment, EventProgress, Execution, ExecutionTarget, ExecutionTrigger, ScheduleProgress, SecretaryConfiguration } from './schema.js';
import type { Document } from './store.js';
import { defaultConfiguration, effectiveRules, executionName, unifiedMainRules, validateAssignment, validateConfiguration, validateExecutionTarget } from './assignment-schema.js';
import { need, same } from './schema.js';
import { messageBatchKind, messageChannel, messageWindowKey, messageWindowMs } from './assignment-message.js';
import type { MessageWindow, MessageWindowState } from './assignment-message.js';

const weekdays = new Map([['Sun', 0], ['Mon', 1], ['Tue', 2], ['Wed', 3], ['Thu', 4], ['Fri', 5], ['Sat', 6]]);
const pageLimit = 100;
const eventSource = (serviceNodeId: string) => 'service:' + serviceNodeId;

export function selectDefaultAgentManager(nodes: Operation.ServiceNode[], hostId: string): Operation.ServiceNode | null {
  const ready = nodes.filter(node => node.serviceName === 'agent-manager' && node.connected && node.synced && node.ready && !node.stale);
  return ready.find(node => node.hostId === hostId) ?? ready[0] ?? null;
}

export class AssignmentScheduler {
  private cursor: string | undefined;
  constructor(readonly engine: SecretaryEngine, readonly fallbackExecution: ExecutionTarget | null = null) {}

  private async operation(...parts: unknown[]): Promise<string> {
    return scopedOperationId(this.engine.client, ['secretary', this.engine.settings.identity.scope.secretaryId, ...parts]);
  }

  private async defaultExecution(): Promise<ExecutionTarget | null> {
    if (this.fallbackExecution) { validateExecutionTarget(this.fallbackExecution); return structuredClone(this.fallbackExecution); }
    const nodes: Operation.ServiceNode[] = []; let cursor: string | undefined;
    do {
      const page = await this.engine.client.request('serviceNodes.list', { serviceName: 'agent-manager', limit: 200, ...(cursor ? { cursor } : {}) });
      nodes.push(...page.items); cursor = page.nextCursor ?? undefined;
    } while (cursor);
    const manager = selectDefaultAgentManager(nodes, this.engine.settings.identity.hostId);
    if (!manager) return null;
    const location = await serviceTools(this.engine.client, manager.serviceNodeId, [{ namespace: 'agent', interfaceVersion: '1.0.0' }]).call('agent.resolveProject', {
      selection: { kind: 'internal', key: 'secretary-' + hashJson(this.engine.settings.identity.scope.secretaryId).slice(7, 23), name: 'Secretary assignments' }
    }) as unknown as Agent.ProjectLocation;
    const target: ExecutionTarget = { serviceNodeId: manager.serviceNodeId, threadCwd: location.cwd, model: null, effort: 'medium', permissions: ':read-only' };
    validateExecutionTarget(target); return target;
  }

  async initialize(): Promise<void> {
    await this.engine.verifyOwner(); const now = this.engine.now().toISOString();
    let configuration = await this.engine.store.named('secretary/configuration', 'configuration');
    if (!configuration) {
      const execution = await this.defaultExecution();
      configuration = await this.engine.store.create('secretary/configuration', 'configuration', defaultConfiguration(this.engine.settings, now, execution), await this.operation('configuration'));
    } else {
      const rules = unifiedMainRules(configuration.value.rules);
      const execution = configuration.value.execution ?? await this.defaultExecution();
      if (!same(rules, configuration.value.rules) || execution && !configuration.value.execution) {
        const value = { ...configuration.value, rules, execution, updatedAt: now };
        validateConfiguration(value); configuration = await this.engine.store.amend(configuration, 'secretary/configuration', value);
      }
    }

  }

  async configuration(): Promise<Document<'secretary/configuration'>> {
    const value = await this.engine.store.named('secretary/configuration', 'configuration');
    if (!value) { await this.initialize(); return (await this.engine.store.named('secretary/configuration', 'configuration'))!; }
    validateConfiguration(value.value); return value;
  }

  private async assignments(): Promise<Document<'secretary/assignment'>[]> {
    const page = await this.engine.client.request('objects.query', { contractKey: 'secretary/assignment', limit: this.engine.settings.recordsPerTick,
      ...(this.cursor ? { cursor: this.cursor } : {}), orderBy: [{ field: 'object.id', direction: 'asc' }],
      where: { op: 'eq', field: 'object.parentId', value: this.engine.store.root } });
    this.cursor = page.nextCursor ?? undefined;
    const result: Document<'secretary/assignment'>[] = [];
    for (const row of page.items) {
      try {
        const assignment = await this.engine.store.read('secretary/assignment', { objectId: row.objectId, revision: row.revision });
        validateAssignment(assignment.value); result.push(assignment);
        this.engine.recoveryIssues.delete('assignment-record:' + row.objectId);
      } catch (error) {
        if (this.engine.signal.aborted) throw error;
        this.engine.issue('assignment-record:' + row.objectId, String((error as { code?: unknown }).code ?? 'secretary_assignment_invalid'));
      }
    }
    return result;
  }

  private occurrences(assignment: Assignment, now: Date, checkedAt: string): { items: ExecutionTrigger[]; checkedAt: string } {
    const trigger = assignment.trigger;
    const since = Date.parse(checkedAt);
    need(Number.isFinite(since), 'secretary_schedule_progress_invalid', 'The retained schedule cursor has an invalid time.');
    if (trigger.kind !== 'schedule') return { items: [], checkedAt: new Date(Math.max(now.getTime(), since)).toISOString() };
    const end = Math.min(now.getTime(), since + 7 * 24 * 60 * 60 * 1000);
    if (since >= end) return { items: [], checkedAt };
    const items: ExecutionTrigger[] = [];
    if (trigger.cadence === 'interval') {
      const interval = trigger.intervalMinutes! * 60_000;
      const first = Math.floor(since / interval) + 1, last = Math.floor(end / interval);
      const through = Math.min(last, first + this.engine.settings.recordsPerTick - 1);
      for (let slot = first; slot <= through; slot++)
        items.push({ kind: 'schedule', key: `interval:${trigger.intervalMinutes}:${slot}`, occurredAt: new Date(slot * interval).toISOString(), payload: { cadence: trigger.cadence, slot } });
      return { items, checkedAt: new Date(through < last ? through * interval : end).toISOString() };
    }
    const format = new Intl.DateTimeFormat('en', { timeZone: trigger.timeZone, year: 'numeric', month: '2-digit', day: '2-digit', weekday: 'short', hour: '2-digit', minute: '2-digit', hourCycle: 'h23', numberingSystem: 'latn' });
    const local = (at: number) => {
      const parts = Object.fromEntries(format.formatToParts(new Date(at)).filter(value => value.type !== 'literal').map(value => [value.type, value.value]));
      return { date: `${parts['year']}-${parts['month']}-${parts['day']}`, time: `${parts['hour']}:${parts['minute']}`, day: weekdays.get(parts['weekday']!) };
    };
    let previous = local(Math.floor(since / 60_000) * 60_000);
    for (let at = (Math.floor(since / 60_000) + 1) * 60_000; at <= end; at += 60_000) {
      const current = local(at), eligible = trigger.cadence === 'daily' || current.day !== undefined && trigger.weekdays.includes(current.day);
      if (eligible && current.time >= trigger.localTime! && (current.date !== previous.date || previous.time < trigger.localTime!))
        items.push({ kind: 'schedule', key: `${trigger.cadence}:${trigger.timeZone}:${current.date}:${trigger.localTime}`, occurredAt: new Date(at).toISOString(),
          payload: { cadence: trigger.cadence, localDate: current.date, localTime: trigger.localTime, timeZone: trigger.timeZone } });
      previous = current;
    }
    return { items, checkedAt: new Date(end).toISOString() };
  }

  private async schedules(assignment: Document<'secretary/assignment'>, configuration: SecretaryConfiguration, now: Date): Promise<void> {
    const name = 'schedule-' + hashJson(assignment.value.assignmentId).slice('sha256:'.length);
    let saved = await this.engine.store.named('secretary/schedule-progress', name);
    if (!saved) {
      const legacy = this.engine.store.technicalNamed<{ updatedAt: string; checkedAt: string }>('secretary/schedule-cursor', assignment.value.assignmentId);
      const original = legacy?.value.updatedAt === assignment.value.updatedAt ? assignment : await this.engine.store.original('secretary/assignment', assignment);
      const checkedAt = original === assignment && legacy ? legacy.value.checkedAt : original.writtenAt ?? original.value.createdAt;
      const progress: ScheduleProgress = { schemaVersion: 1, assignmentId: assignment.value.assignmentId, assignment: original.pin,
        assignmentSnapshot: structuredClone(original.value), checkedAt, updatedAt: now.toISOString() };
      saved = await this.engine.store.create('secretary/schedule-progress', name, progress, await this.operation('schedule-progress', assignment.value.assignmentId));
      if (legacy) this.engine.store.technicalDelete('secretary/schedule-cursor', legacy.pin.objectId);
    }
    need(saved.value.assignmentId === assignment.value.assignmentId && saved.value.assignment.objectId === assignment.pin.objectId,
      'secretary_schedule_progress_mismatch', 'The durable schedule cursor belongs to another assignment.');
    for (let page = 0; page < 4; page++) {
      const progress = saved.value;
      if (progress.assignment.revision > assignment.pin.revision) return;
      const next = progress.assignment.revision < assignment.pin.revision
        ? await this.engine.store.read('secretary/assignment', { objectId: assignment.pin.objectId, revision: progress.assignment.revision + 1 }) : null;
      const transitionAt = next ? Date.parse(next.writtenAt ?? next.value.updatedAt) : Infinity;
      const canSwitch = next !== null && transitionAt <= now.getTime();
      const through = canSwitch ? new Date(transitionAt) : now;
      const result = progress.assignmentSnapshot.enabled
        ? this.occurrences(progress.assignmentSnapshot, through, progress.checkedAt)
        : { items: [] as ExecutionTrigger[], checkedAt: new Date(Math.max(Date.parse(progress.checkedAt), through.getTime())).toISOString() };
      const original = { pin: progress.assignment, value: progress.assignmentSnapshot };
      for (const occurrence of result.items) await this.enqueue(original, occurrence, configuration);
      const switched = canSwitch && Date.parse(result.checkedAt) >= transitionAt;
      const nextValue: ScheduleProgress = switched ? { ...progress, assignment: next.pin, assignmentSnapshot: structuredClone(next.value),
        checkedAt: new Date(Math.max(Date.parse(result.checkedAt), transitionAt)).toISOString(), updatedAt: now.toISOString() }
        : { ...progress, checkedAt: result.checkedAt, updatedAt: now.toISOString() };
      const progressed = Date.parse(nextValue.checkedAt) - Date.parse(progress.checkedAt);
      if (!switched && result.items.length === 0 && progressed < 60 * 60 * 1000) return;
      saved = await this.engine.store.amend(saved, 'secretary/schedule-progress', nextValue);
      if (!switched && Date.parse(result.checkedAt) >= through.getTime()) return;
    }
  }

  private eventMatches(assignment: Assignment, event: Operation.Event): boolean {
    const trigger = assignment.trigger; if (trigger.kind !== 'event' || event.topic !== trigger.topic || event.topicVersion !== trigger.topicVersion) return false;
    if (event.topic === 'hive.object.changed' && event.source === 'principal:' + this.engine.settings.identity.principalId) return false;
    if (trigger.sourceServiceNodeId && event.source !== eventSource(trigger.sourceServiceNodeId)) return false;
    const payload = event.payload && typeof event.payload === 'object' && !Array.isArray(event.payload) ? event.payload as Record<string, Wire.Json> : {};
    return !trigger.eventKind || payload['kind'] === trigger.eventKind;
  }

  private async enqueue(assignment: Document<'secretary/assignment'>, trigger: ExecutionTrigger, configuration: SecretaryConfiguration): Promise<Document<'secretary/execution'>> {
    if (!configuration.execution) throw Object.assign(new Error('Secretary execution target is not configured.'), { code: 'secretary_execution_unconfigured' });
    const name = executionName(assignment.value.assignmentId, trigger.key), existing = await this.engine.store.named('secretary/execution', name);
    if (existing) return existing;
    const executionTarget = structuredClone(configuration.execution);
    executionTarget.model = assignment.value.execution?.model ?? executionTarget.model;
    executionTarget.effort = assignment.value.execution?.effort ?? executionTarget.effort;
    const now = this.engine.now().toISOString(), execution: Execution = { schemaVersion: 1, executionId: hashJson([assignment.value.assignmentId, trigger.key]), assignment: assignment.pin,
      assignmentSnapshot: structuredClone(assignment.value), trigger: structuredClone(trigger), effectiveRules: effectiveRules(configuration.rules, assignment.value.rules), executionTarget, phase: 'queued',
      serviceNodeId: configuration.execution.serviceNodeId, nativeTarget: null, threadId: null, turnId: null, nativeOperations: { threadStart: null, turnStart: null, archive: null, delete: null }, preflightOutput: null, result: null, errorCode: null,
      createdAt: now, startedAt: null, completedAt: null, archivedAt: null, deleteAfter: null, deletedAt: null, updatedAt: now };
    return await this.engine.store.create('secretary/execution', name, execution, await this.operation('execution', assignment.value.assignmentId, trigger.key));
  }

  private async flushMessageWindow(saved: { pin: { objectId: string; revision: number }; value: MessageWindowState }, configuration: SecretaryConfiguration): Promise<void> {
    const window = saved.value.pending;
    if (!window) return;
    await this.enqueue({ pin: window.assignment, value: window.assignmentSnapshot }, {
      kind: 'event', key: `message-batch:${window.firstSequence}:${window.eventIds[0]}`,
      occurredAt: window.firstMessageAt,
      payload: { kind: messageBatchKind, dueAt: window.dueAt, serviceNodeId: window.serviceNodeId,
        channel: window.channel as unknown as Wire.Json, itemIds: window.itemIds, eventIds: window.eventIds },
    }, configuration);
    this.engine.store.technicalAmend(messageWindowKey, saved, { ...saved.value, pending: null });
  }

  private async collectMessageEvent(assignment: Document<'secretary/assignment'>, event: Operation.Event,
    configuration: SecretaryConfiguration): Promise<void> {
    const payload = event.payload && typeof event.payload === 'object' && !Array.isArray(event.payload)
      ? event.payload as Record<string, Wire.Json> : {};
    const eventId = payload['eventId'], channel = messageChannel(payload);
    need(typeof eventId === 'string' && channel && event.source.startsWith('service:'),
      'secretary_message_event_invalid', 'The message event has no source identity or channel.');
    const serviceNodeId = event.source.slice('service:'.length);
    const itemId = channel.itemId;
    const name = hashJson([assignment.value.assignmentId, serviceNodeId, channel.key]);
    let saved = this.engine.store.technicalNamed<MessageWindowState>(messageWindowKey, name);
    if (saved && event.sequence <= saved.value.lastSequence) return;
    if (saved?.value.pending) need(same(saved.value.pending.channel.reference, channel.reference),
      'secretary_message_channel_conflict', 'The message channel reference changed within one window.');
    const firstMessageAt = channel.occurredAt;
    const occurredAt = Date.parse(firstMessageAt);
    need(Number.isFinite(occurredAt), 'secretary_message_event_invalid', 'The message time is invalid.');
    if (saved?.value.pending && occurredAt >= Date.parse(saved.value.pending.dueAt)) {
      await this.flushMessageWindow(saved, configuration);
      saved = this.engine.store.technicalNamed<MessageWindowState>(messageWindowKey, name);
    }
    const pending: MessageWindow = saved?.value.pending
      ? { ...saved.value.pending, itemIds: itemId ? [...saved.value.pending.itemIds, itemId] : saved.value.pending.itemIds,
          eventIds: [...saved.value.pending.eventIds, eventId] }
      : { assignment: assignment.pin, assignmentSnapshot: structuredClone(assignment.value), firstSequence: event.sequence,
          firstMessageAt, dueAt: new Date(occurredAt + messageWindowMs).toISOString(), serviceNodeId,
          channel, itemIds: itemId ? [itemId] : [], eventIds: [eventId] };
    const value: MessageWindowState = { assignmentId: assignment.value.assignmentId, lastSequence: event.sequence, pending };
    if (saved) this.engine.store.technicalAmend(messageWindowKey, saved, value);
    else this.engine.store.technicalCreate(messageWindowKey, name, value);
  }

  private async flushDueMessageWindows(assignment: Document<'secretary/assignment'>, configuration: SecretaryConfiguration, now: Date): Promise<void> {
    for (const saved of this.engine.store.technicalList<MessageWindowState>(messageWindowKey))
      if (saved.value.assignmentId === assignment.value.assignmentId && saved.value.pending && Date.parse(saved.value.pending.dueAt) <= now.getTime())
        await this.flushMessageWindow(saved, configuration);
  }

  private async revisionAt(assignment: Document<'secretary/assignment'>, at: string): Promise<Document<'secretary/assignment'> | null> {
    for (let revision = assignment.pin.revision; revision >= 1; revision--) {
      const candidate = revision === assignment.pin.revision ? assignment
        : await this.engine.store.read('secretary/assignment', { objectId: assignment.pin.objectId, revision });
      if (Date.parse(candidate.writtenAt ?? candidate.value.updatedAt) <= Date.parse(at)) return candidate;
    }
    return null;
  }

  private async events(assignment: Document<'secretary/assignment'>, configuration: SecretaryConfiguration, budget: number): Promise<boolean> {
    const key = assignment.value.assignmentId;
    const name = 'secretary-assignment-' + hashJson(key).slice('sha256:'.length, 48);
    const progressName = 'event-' + hashJson(key).slice('sha256:'.length);
    let progress = await this.engine.store.named('secretary/event-progress', progressName);
    if (!progress && assignment.pin.revision === 1 && assignment.value.trigger.kind !== 'event') return true;
    for (let page = 0; page < budget; page++) {
      const batch = await this.engine.client.request('events.subscribe', { name, filter: {}, initialSequence: 0, durable: true, limit: pageLimit });
      if (!progress) {
        need(batch.gap || batch.throughSequence === 0 || batch.items[0]?.sequence === 1,
          'secretary_event_progress_missing', 'The event subscription advanced without its durable assignment progress.');
        const original = await this.engine.store.original('secretary/assignment', assignment);
        const value: EventProgress = { schemaVersion: 1, assignmentId: key, assignment: original.pin,
          assignmentSnapshot: structuredClone(original.value), processedThrough: 0, active: false, needsRebase: false, updatedAt: this.engine.now().toISOString() };
        progress = await this.engine.store.create('secretary/event-progress', progressName, value, await this.operation('event-progress', key));
      }
      need(progress.value.assignmentId === key && progress.value.assignment.objectId === assignment.pin.objectId,
        'secretary_event_progress_mismatch', 'The durable event cursor belongs to another assignment.');
      need(progress.value.processedThrough <= batch.throughSequence, 'secretary_event_progress_conflict', 'The event subscription moved behind its durable progress.');
      if (progress.value.processedThrough < batch.throughSequence) {
        let next = structuredClone(progress.value);
        let stateChanged = false;
        if (batch.gap) {
          const prior = this.engine.store.technicalNamed<{ prunedThroughSequence: number }>('secretary/event-gap', key);
          const value = { prunedThroughSequence: batch.gap.prunedThroughSequence };
          if (prior) this.engine.store.technicalAmend('secretary/event-gap', prior, value);
          else this.engine.store.technicalCreate('secretary/event-gap', key, value);
          this.engine.issue('event-gap:' + key, 'secretary_event_gap');
          next.needsRebase = true;
          stateChanged = true;
        }
        if (next.needsRebase && batch.items.length) {
          const rebased = await this.revisionAt(assignment, batch.items[0]!.occurredAt);
          next.assignment = rebased?.pin ?? next.assignment;
          next.assignmentSnapshot = structuredClone(rebased?.value ?? next.assignmentSnapshot);
          next.active = rebased !== null;
          next.needsRebase = false;
          stateChanged = true;
        }
        for (const event of batch.items) {
          if (event.sequence <= progress.value.processedThrough) continue;
          if (next.active && next.assignmentSnapshot.enabled && this.eventMatches(next.assignmentSnapshot, event)) {
            const selected = { pin: next.assignment, value: next.assignmentSnapshot };
            if (event.source.startsWith('service:') && messageChannel(event.payload)) await this.collectMessageEvent(selected, event, configuration);
            else await this.enqueue(selected,
              { kind: 'event', key: `event:${event.sequence}:${event.mutationId}`, occurredAt: event.occurredAt,
                payload: { topic: event.topic, topicVersion: event.topicVersion, source: event.source, mutationId: event.mutationId, data: event.payload } }, configuration);
          }
          if (event.topic === 'hive.object.changed' && event.payload && typeof event.payload === 'object' && !Array.isArray(event.payload)
            && (event.payload as Record<string, Wire.Json>)['objectId'] === assignment.pin.objectId) {
            const revision = Number((event.payload as Record<string, Wire.Json>)['revision']);
            if (Number.isSafeInteger(revision) && revision > next.assignment.revision) {
              const changed = await this.engine.store.read('secretary/assignment', { objectId: assignment.pin.objectId, revision });
              next.assignment = changed.pin; next.assignmentSnapshot = structuredClone(changed.value); next.active = true;
              stateChanged = true;
            } else if (revision === next.assignment.revision && !next.active) { next.active = true; stateChanged = true; }
          }
        }
        if (stateChanged) {
          next.processedThrough = batch.throughSequence;
          next.updatedAt = this.engine.now().toISOString();
          progress = await this.engine.store.amend(progress, 'secretary/event-progress', next);
        }
      }
      await this.engine.client.request('events.ack', { name, throughSequence: batch.throughSequence,
        ...(batch.gap ? { gapThroughSequence: batch.gap.prunedThroughSequence } : {}) });
      if (page === 0) {
        const legacy = this.engine.store.technicalNamed<{ name: string }>('secretary/event-subscription', key);
        if (legacy && legacy.value.name !== name) {
          await this.engine.client.request('events.unsubscribe', { name: legacy.value.name, mutationId: await newOperationId(this.engine.client) });
          this.engine.store.technicalDelete('secretary/event-subscription', legacy.pin.objectId);
        }
      }
      if (!batch.hasMore) return true;
    }
    return false;
  }

  async tick(): Promise<void> {
    await this.engine.verifyOwner(); let configuration = await this.configuration(); const now = this.engine.now();
    if (!configuration.value.execution) {
      const execution = await this.defaultExecution();
      if (execution) configuration = await this.engine.store.amend(configuration, 'secretary/configuration', { ...configuration.value, execution, updatedAt: now.toISOString() });
    }
    const assignments = await this.assignments(), eventBudget = Math.max(1, Math.floor(32 / Math.max(1, assignments.length)));
    for (const assignment of assignments) {
      const key = 'assignment:' + assignment.value.assignmentId;
      try {
        const gap = this.engine.store.technicalNamed('secretary/event-gap', assignment.value.assignmentId);
        if (gap) this.engine.issue('event-gap:' + assignment.value.assignmentId, 'secretary_event_gap');
        const eventsCaughtUp = await this.events(assignment, configuration.value, eventBudget);
        if (eventsCaughtUp) await this.flushDueMessageWindows(assignment, configuration.value, now);
        await this.schedules(assignment, configuration.value, now);
        this.engine.recoveryIssues.delete(key);
      } catch (error) {
        if (this.engine.signal.aborted) throw error;
        const code = typeof (error as { code?: unknown }).code === 'string' ? String((error as { code: string }).code) : 'secretary_assignment_failed';
        this.engine.issue(key, code);
      }
    }
  }
}
