import { ignored } from './policy.js';
import { hashJson } from '../../../packages/sdk/src/node.js';
import { need, same } from './schema.js';
import type { CaptureRequest, CheckpointRequest, Pin } from './schema.js';
import type { SecretaryEngine } from './engine.js';
import { messageHash, identityHash, sourceIdentity } from './engine.js';
import { mutation } from './store.js';
import { TeamsNative, teamsName } from './teams-native.js';
import type { TeamsHeadDocument, TeamsPlanDocument } from './teams-native.js';
import type { MicrosoftDocument } from './microsoft-store.js';
import type { TeamsCollector } from './teams-schema.js';

type Evidence = MicrosoftDocument<'secretary/teams-evidence'>;
type Publication = MicrosoftDocument<'secretary/teams-publication'>;
const check = (value: unknown, code = 'microsoft_publication_invalid') => need(value, code, 'Teams publication requires exact original Secretary captures and checkpoint.');
/** One retained read page, then exact Secretary captures and one confirmed source checkpoint. */
export class TeamsSource extends TeamsNative {
  private notBefore: string | null = null;
  constructor(engine: SecretaryEngine, configuration: TeamsCollector) { super(engine, configuration); }
  private async requestId(proof: Evidence, scope: unknown): Promise<string> {
    check(proof.value.calls[0]); const call = await this.store.read('secretary/teams-call', proof.value.calls[0]!.objectId);
    return mutation(call.value.operationId, scope);
  }
  private async captureRequest(proof: Evidence, index: number): Promise<CaptureRequest> {
    check(proof.value.page?.messages[index]);
    return { action: 'capture', operationId: await this.requestId(proof, ['teams-capture', proof.pin, index]), expectedScope: this.engine.settings.identity.scope, sourceId: this.configuration.sourceId, message: proof.value.page!.messages[index]! };
  }
  private async checkpointRequest(proof: Evidence, plan: TeamsPlanDocument): Promise<CheckpointRequest> {
    check(proof.value.page); const captures = await Promise.all(proof.value.page!.messages.map((_message, index) => this.captureRequest(proof, index)));
    return { action: 'checkpoint', operationId: await this.requestId(proof, ['teams-checkpoint', proof.pin]), expectedScope: this.engine.settings.identity.scope,
      sourceId: this.configuration.sourceId, previousCursor: plan.value.previousCursor, nextCursor: 'teams-page:' + hashJson(proof.pin).slice(7), captures: captures.map(value => value.operationId) };
  }
  private async confirmed(pin: Pin, request: CaptureRequest | CheckpointRequest) {
    const operation = await this.engine.store.read('secretary/operation', pin), caller = this.engine.settings.identity.principalId;
    const actual = await this.engine.find(caller, request.operationId);
    check(actual && same(actual.pin, pin) && operation.value.callerPrincipalId === caller && operation.value.requestHash === hashJson(request) && operation.value.phase === 'succeeded');
    const originalSource = { value: operation.value.source };
    if (request.action === 'capture' && operation.value.ignoredReason) {
      check(!operation.value.effect && !operation.value.request && !operation.value.prepared && ignored(originalSource.value.definition, request.message) === operation.value.ignoredReason); return operation;
    }
    if (!operation.value.effect) throw Error('Missing original effect');
    if (request.action === 'capture') {
      check(!operation.value.ignoredReason); const item = await this.engine.item(operation.value.effect);
      check(item.value.sourceId === request.sourceId && item.value.identityHash === identityHash(request.sourceId, request.message) && item.value.messageHash === messageHash(request.message));
    } else {
      const source = await this.engine.source(request.sourceId);
      check(source.pin.objectId === operation.value.effect.objectId && same(sourceIdentity(source.value.definition), sourceIdentity(originalSource.value.definition)));
    }
    return operation;
  }
  private async publication(proof: Evidence, plan: TeamsPlanDocument, create = true): Promise<Publication> {
    check(!proof.value.gap && proof.value.page);
    const value = create ? await this.store.create('secretary/teams-publication', teamsName('publication', proof.pin), { schemaVersion: 1, evidence: proof.pin, captured: [], checkpoint: null }) : await this.store.find('secretary/teams-publication', teamsName('publication', proof.pin));
    check(value); if (!value) throw Error('Unreachable');
    check(same(value.value.evidence, proof.pin) && value.value.captured.length <= proof.value.page!.messages.length &&
      value.pin.revision === value.value.captured.length + 1 + (value.value.checkpoint ? 1 : 0));
    for (const [index, pin] of value.value.captured.entries()) await this.confirmed(pin, await this.captureRequest(proof, index));
    if (value.value.checkpoint) { check(value.value.captured.length === proof.value.page!.messages.length); await this.confirmed(value.value.checkpoint, await this.checkpointRequest(proof, plan)); }
    return value;
  }
  async head(): Promise<TeamsHeadDocument> {
    const head = await this.rawHead();
    if (head.value.pending) {
      const plan = await this.plan(head.value.pending); check(plan.value.head.objectId === head.pin.objectId);
    } else if (head.value.lastPage) {
      const proof = await this.evidence(head.value.lastPage), plan = await this.plan(proof.value.plan);
      check(plan.value.head.objectId === head.pin.objectId);
      if (!proof.value.gap) check((await this.publication(proof, plan, false)).value.checkpoint);
    }
    return head;
  }
  private async publish(proof: Evidence, plan: TeamsPlanDocument): Promise<boolean> {
    const publication = await this.publication(proof, plan); if (publication.value.checkpoint) return true;
    const request = publication.value.captured.length < proof.value.page!.messages.length ? await this.captureRequest(proof, publication.value.captured.length) : await this.checkpointRequest(proof, plan);
    const caller = this.engine.settings.identity.principalId; let saved = await this.engine.find(caller, request.operationId);
    if (!saved || saved.value.phase !== 'succeeded') {
      // An accepted checkpoint can already have committed its effect before its receipt was saved.
      // Its original prepared revision and mutation identity govern recovery in SecretaryEngine.
      if (!saved) await this.guard(plan);
      await this.engine.action(caller, request); saved = await this.engine.find(caller, request.operationId);
    }
    check(saved?.value.phase === 'succeeded', 'microsoft_secretary_pending'); if (!saved) throw Error('Unreachable'); await this.confirmed(saved.pin, request);
    await this.store.amend('secretary/teams-publication', publication, { ...publication.value,
      ...(request.action === 'capture' ? { captured: [...publication.value.captured, saved.pin] } : { checkpoint: saved.pin }) }); return request.action === 'checkpoint';
  }
  async step(): Promise<'idle' | 'pending' | 'page' | 'complete' | 'gap'> {
    if (this.notBefore && this.engine.now().toISOString() < this.notBefore) return 'idle';
    this.notBefore = null;
    await this.engine.verifyOwner(); let head = await this.head(); const plan = await this.prepare(head); if (!plan) { this.notBefore = head.value.nextPollAt ?? this.configuration.since; return 'idle'; }
    let proof = await this.store.find('secretary/teams-evidence', teamsName('evidence', plan.pin), true);
    proof = proof ? await this.evidence(proof.pin) : await this.collect(plan); if (!proof) return 'pending';
    if (!proof.value.gap && !await this.publish(proof, plan)) return 'pending';
    head = await this.rawHead(); check(same(head.value.pending, plan.pin), 'microsoft_plan_changed');
    const next = this.nextHead(head.value, plan, proof); await this.store.amend('secretary/teams-head', head, next);
    this.notBefore = next.continuation ? null : next.nextPollAt;
    return proof.value.gap ? 'gap' : next.continuation ? 'page' : 'complete';
  }
}
