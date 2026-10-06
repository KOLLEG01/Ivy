import { canonical, encodeJson, hashJson } from '../../../packages/sdk/src/node.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import { validateAgent, validateAgentFrame } from '../../../packages/sdk/src/node.js';
import { validateShared } from '../../../packages/sdk/src/node.js';
import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import { nativeFrameBytes, nativeRequestFrameBytes, nativeAnswerFrameBytes } from './limits.js';
import { managementFrameBytes } from '../../../packages/sdk/src/node.js';
export { nativeFrameBytes } from './limits.js';
const outcomeReservation = nativeFrameBytes + 65536;
const completed = new Set<Agent.Operation['phase']>(['succeeded','failed','outcome_unknown']);
type Identity = Pick<Agent.Operation,'callerPrincipalId'|'operationId'>;
type Owner = Pick<Agent.Operation,'serviceNodeId'|'nativeVersion'|'nativeExecutableHash'> & {hostId:string};

/** Bounded connection state only. Durable caller receipts live in Hive. */
export class NativeJournal {
  private operations = new Map<string,{value:Agent.Operation;charge:number}>();
  private pending = new Map<string,{value:Agent.PendingInput;charge:number}>();
  private observations = new Map<string,Agent.Status['observedMethods'][number]>();
  private revisions = new Map<string,number>();
  private currentEpoch: string|null = null;
  private durable = new Set<string>();
  onTerminal: ((identity: Identity) => void) | undefined;
  constructor(readonly owner:Owner, readonly limits:Agent.Settings['limits']) {
    validateShared('Identifier',owner.serviceNodeId); validateShared('Hash',owner.nativeExecutableHash);
  }
  close():void { this.operations.clear();this.pending.clear();this.observations.clear();this.revisions.clear();this.currentEpoch=null;this.durable.clear();this.onTerminal=undefined; }
  private transaction<T>(action:()=>T):T { return action(); }
  private key({callerPrincipalId,operationId}:Identity):string { validateShared('Identifier',callerPrincipalId);validateShared('Identifier',operationId);return canonical({callerPrincipalId,operationId}); }
  get epoch():string|null { return this.currentEpoch; }
  inventoryRevision(kind:'thread'|'project',minimum=0):number { const next=Math.max(this.revisions.get(kind)??0,minimum)+1;this.revisions.set(kind,next);return next; }
  status():Agent.Status['operations'] { return {retained:this.operations.size,maximum:this.limits.maxOperations,bytes:[...this.operations.values(),...this.pending.values()].reduce((n,row)=>n+row.charge,0),maximumBytes:this.limits.maxJournalBytes}; }
  private compact():void {
    for(const [key,row] of this.operations) if(completed.has(row.value.phase) && (this.durable.has(key) || row.value.callerPrincipalId==='ivy-native-clock')) {this.operations.delete(key);this.durable.delete(key);}
    for(const [key,row] of this.pending) if(!['pending','answering'].includes(row.value.state)) this.pending.delete(key);
  }
  get(identity:Identity):Agent.Operation|null { return structuredClone(this.operations.get(this.key(identity))?.value??null); }
  forget(identity:Identity):void { this.operations.delete(this.key(identity)); }
  retainedInHive(identity:Identity):void { this.durable.add(this.key(identity)); }
  private existing(identity:Identity):Agent.Operation { const value=this.get(identity);requireThat(value,'not_found','Operation is not in this connection.');return value; }
  private save(value:Agent.Operation):Agent.Operation {
    const key=this.key(value),row=this.operations.get(key),size=Buffer.byteLength(encodeJson(value));
    requireThat(row && size<=row.charge,'native_journal_capacity','Outcome exceeds reserved memory.');
    this.operations.set(key,{value:structuredClone(value),charge:completed.has(value.phase)?size:row.charge});
    if(completed.has(value.phase)) {
      const observed=this.observations.get(value.method)??{method:value.method,lastSucceededAt:null,lastFailedAt:null,lastCode:null};
      if(value.phase==='succeeded') observed.lastSucceededAt=value.updatedAt; else {observed.lastFailedAt=value.updatedAt;observed.lastCode=value.code;}
      this.observations.set(value.method,observed); if(this.observations.size>256)this.observations.delete(this.observations.keys().next().value!);
      this.onTerminal?.(value);
    }
    return value;
  }
  accept(identity:Identity,method:string,params:Wire.Json):{operation:Agent.Operation;created:boolean} { return this.admit(identity,method,params,false); }
  prevent(identity:Identity,method:string,params:Wire.Json):Agent.Operation { return this.admit(identity,method,params,true).operation; }
  private admit(identity:Identity,method:string,params:Wire.Json,prevent:boolean):{operation:Agent.Operation;created:boolean} {
    if(method==='agent.answer') {requireThat(params && typeof params==='object' && !Array.isArray(params),'invalid_arguments','Answer requires its exact identity.');validateAgent('AnswerInput',{operationId:identity.operationId,...params}); const answer=params as unknown as Omit<Agent.AnswerInput,'operationId'>;encodeJson({id:answer.identity.requestId,...answer.reply},nativeAnswerFrameBytes);}
    else encodeJson({id:'00000000-0000-0000-0000-000000000000',method,params},nativeRequestFrameBytes);
    const requestHash=hashJson({method,params}),prior=this.get(identity);
    if(prior){requireThat(prior.requestHash===requestHash,'mutation_conflict','Operation identity has different arguments.');return {operation:prior,created:false};}
    this.compact();
    const now=new Date().toISOString(),value:Agent.Operation={schemaVersion:1,...identity,serviceNodeId:this.owner.serviceNodeId,nativeVersion:this.owner.nativeVersion,nativeExecutableHash:this.owner.nativeExecutableHash,method,params,requestHash,phase:prevent?'failed':'accepted',createdAt:now,updatedAt:now,epoch:null,requestId:null,reply:null,code:prevent?'request_prevented':null};
    const charge=Buffer.byteLength(encodeJson(value))+(prevent?0:outcomeReservation),current=this.status();
    const cleanup=prevent||['thread/unsubscribe','turn/interrupt','agent.answer'].includes(method);
    requireThat(current.retained+(cleanup?0:4)<current.maximum && current.bytes+charge+(cleanup?0:outcomeReservation+65536)<=current.maximumBytes,'native_journal_capacity','Connection memory is full; no native request sent.');
    this.operations.set(this.key(identity),{value:structuredClone(value),charge});return {operation:value,created:true};
  }
  /** Called once for each newly owned process, before initialization or caller dispatch. */
  beginEpoch(epoch: string): void {
    validateAgent('InputIdentity', { serviceNodeId: this.owner.serviceNodeId, epoch, requestId: 0 });
    this.transaction(() => {
      requireThat(epoch !== this.epoch, 'mutation_conflict', 'A new native connection must have a fresh epoch.');
      this.loseUnfinished('native_epoch_lost'); this.retireInputs('native_epoch_lost'); this.currentEpoch = epoch;
    });
  }
  private loseUnfinished(code: string): void {
    for (const {value: original} of this.operations.values()) {
      if (!['accepted','dispatched'].includes(original.phase)) continue;
      const value = structuredClone(original);
      value.phase = value.phase === 'accepted' ? 'failed' : 'outcome_unknown';
      value.code = value.phase === 'failed' ? 'request_not_dispatched' : code;
      value.updatedAt = new Date().toISOString(); this.save(value);
    }
  }
  loseEpoch(epoch: string, code: string): void {
    this.transaction(() => { if (this.epoch !== epoch) return; this.loseUnfinished(code); this.retireInputs(code); this.currentEpoch = null; });
  }
  dispatch(identity: Identity, epoch: string, requestId: Agent.RequestId): Agent.Operation {
    return this.transaction(() => {
      const value = this.existing(identity);
      requireThat(value.phase === 'accepted' && epoch === this.epoch && Boolean(epoch), 'native_state_conflict', 'Native dispatch requires a new accepted operation and the current epoch.');
      return this.save({ ...value, phase: 'dispatched', epoch, requestId, updatedAt: new Date().toISOString() });
    });
  }
  finish(identity: Identity, epoch: string, requestId: Agent.RequestId, reply: Agent.Reply): Agent.Operation {
    encodeJson({ id: requestId, ...reply }, nativeFrameBytes); validateAgentFrame(reply);
    return this.transaction(() => {
      const value = this.existing(identity);
      requireThat(value.phase === 'dispatched' && epoch === this.epoch && value.epoch === epoch && value.requestId === requestId,
        'native_state_conflict', 'Only the exact dispatched request in the current epoch can record a native result.');
      return this.save({ ...value, phase: 'result' in reply ? 'succeeded' : 'failed', reply,
        code: 'result' in reply ? null : 'native_error', updatedAt: new Date().toISOString() });
    });
  }
  rejectBeforeDispatch(identity: Identity, code: string): Agent.Operation {
    return this.transaction(() => { const value = this.existing(identity); requireThat(value.phase === 'accepted', 'native_state_conflict', 'A dispatched native effect cannot be rejected as unexecuted.');
      return this.save({ ...value, phase: 'failed', code, updatedAt: new Date().toISOString() }); });
  }
  private inputKey(identity: Agent.InputIdentity): string {
    validateAgent('InputIdentity', identity); requireThat(identity.serviceNodeId === this.owner.serviceNodeId, 'target_conflict', 'Native input belongs to another Service Node.');
    return hashJson(identity);
  }
  input(identity: Agent.InputIdentity): Agent.PendingInput | null { return structuredClone(this.pending.get(this.inputKey(identity))?.value ?? null); }
  pendingInputCount(): number { return [...this.pending.values()].filter(row => ['pending','answering'].includes(row.value.state)).length; }
  observeInput(identity: Agent.InputIdentity, method: string, params: Wire.Json): Agent.PendingInput {
    encodeJson({ id: identity.requestId, method, params }, nativeFrameBytes);
    this.compact();
    return this.transaction(() => {
      requireThat(identity.epoch === this.epoch, 'native_state_conflict', 'Input must come from the current native connection.');
      const prior = this.input(identity);
      if (prior) {
        requireThat(prior.method === method && canonical(prior.params) === canonical(params) && prior.state === 'pending', 'native_input_conflict', 'Native request ID was reused or its original payload changed.'); return prior;
      }
      const count = this.pending.size;
      requireThat(count < this.limits.maxOperations && this.pendingInputCount() < this.limits.maxPendingInputs, 'native_input_capacity', 'Native pending input/history capacity is full.');
      const fields = params !== null && typeof params === 'object' && !Array.isArray(params) ? params : {};
      const thread = fields['threadId'] ?? fields['conversationId'], turn = fields['turnId'], now = new Date().toISOString();
      const value: Agent.PendingInput = { identity, method, params, observedAt: now, updatedAt: now, threadId: typeof thread === 'string' ? thread : null,
        turnId: typeof turn === 'string' ? turn : null, state: 'pending', answerOperationId: null, answerCallerPrincipalId: null, reply: null, code: null };
      const encoded = encodeJson(value), charge = Buffer.byteLength(encoded) + outcomeReservation;
      requireThat(this.status().bytes + charge + outcomeReservation + 65536 <= this.limits.maxJournalBytes, 'native_input_capacity', 'Native journal cannot reserve this pending input, its answer and cleanup.');
      this.pending.set(this.inputKey(identity), {value: structuredClone(value), charge}); return value;
    });
  }
  private saveInput(value: Agent.PendingInput): Agent.PendingInput {
    const key = this.inputKey(value.identity), row = this.pending.get(key), size = Buffer.byteLength(encodeJson(value));
    requireThat(row && size <= row.charge, 'native_input_capacity', 'Input result exceeds its reserved memory.');
    this.pending.set(key, {value: structuredClone(value), charge: ['pending','answering'].includes(value.state) ? row.charge : size});
    return value;
  }
  /** Called inside the native answer's beforeSend hook after exact native output validation. */
  dispatchAnswer(operation: Identity, identity: Agent.InputIdentity, reply: Agent.Reply): Agent.PendingInput {
    validateAgentFrame(reply);
    return this.transaction(() => {
      const value = this.input(identity), accepted = this.existing(operation);
      requireThat(value && value.state === 'pending' && identity.epoch === this.epoch, 'native_input_expired', 'Only the exact pending input in the current native connection can be answered.');
      requireThat(accepted.phase === 'accepted' && accepted.method === 'agent.answer' && accepted.requestHash === hashJson({ method: 'agent.answer', params: { identity, reply } }),
        'mutation_conflict', 'Answer dispatch must match its accepted original caller operation.');
      const now = new Date().toISOString();
      this.save({ ...accepted, phase: 'dispatched', epoch: identity.epoch, requestId: identity.requestId, updatedAt: now });
      return this.saveInput({ ...value, state: 'answering', answerOperationId: operation.operationId, answerCallerPrincipalId: operation.callerPrincipalId, reply, updatedAt: now });
    });
  }
  resolveInput(epoch: string, requestId: Agent.RequestId, threadId: string): Agent.PendingInput | null {
    return this.transaction(() => {
      requireThat(epoch === this.epoch, 'native_state_conflict', 'Input resolution must come from the current native connection.');
      const value = this.input({ serviceNodeId: this.owner.serviceNodeId, epoch, requestId });
      if (!value) return null; // A native request may have been resolved entirely inside the native host.
      requireThat(value.threadId === null || value.threadId === threadId, 'native_input_conflict', 'Native input resolution names a different thread.');
      if (!['pending', 'answering'].includes(value.state)) return value;
      const now = new Date().toISOString();
      if (value.state === 'answering') {
        const operation = this.existing({ callerPrincipalId: value.answerCallerPrincipalId!, operationId: value.answerOperationId! });
        requireThat(operation.phase === 'dispatched' && operation.epoch === epoch && operation.requestId === requestId, 'native_state_conflict', 'Native answer outcome has no matching dispatched intent.');
        this.save({ ...operation, phase: 'succeeded', reply: { result: { nativeRequestResolved: true } }, code: null, updatedAt: now });
      }
      return this.saveInput({ ...value, state: value.state === 'answering' ? 'answered' : 'expired', code: value.state === 'answering' ? null : 'native_request_resolved', updatedAt: now });
    });
  }
  private retireInputs(code: string): void {
    for (const {value} of this.pending.values()) if (['pending','answering'].includes(value.state))
      this.saveInput({...value, state: value.state === 'answering' ? 'outcome_unknown' : 'expired', code, updatedAt: new Date().toISOString()});
  }
  inputs(query: Agent.PendingInputQuery): Agent.PendingInputPage {
    validateAgent('PendingInputQuery', query);
    if (query.identity) { const value = this.input(query.identity); return {epoch:this.epoch,items:value?[value]:[],truncated:false}; }
    const values = [...this.pending.values()].map(row=>row.value).filter(value=>query.includeExpired || ['pending','answering'].includes(value.state))
      .sort((a,b)=>Number(!['pending','answering'].includes(a.state))-Number(!['pending','answering'].includes(b.state)) || b.updatedAt.localeCompare(a.updatedAt));
    const items: Agent.PendingInput[]=[]; let bytes=0;
    for(const value of values) { const size=Buffer.byteLength(encodeJson(value)); if(items.length >= (query.limit??128) || bytes+size>managementFrameBytes-1024*1024) break; items.push(structuredClone(value));bytes+=size; }
    return {epoch:this.epoch,items,truncated:items.length<values.length};
  }
  reserveNotificationSequence(): {start:number;end:number} { return {start:0,end:Number.MAX_SAFE_INTEGER}; }
  observedMethods(): Agent.Status['observedMethods'] { return structuredClone([...this.observations.values()].slice(-256)); }
}
