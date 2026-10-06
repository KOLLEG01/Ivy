import { hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import { validateAgent } from '../../../packages/sdk/src/node.js';
import type { Agent, Wire } from '../../../packages/sdk/src/node.js';
import { LocalAgentState } from './hive-state.js';
import { NativeJournal } from './journal.js';

type Identity=Pick<Agent.Operation,'callerPrincipalId'|'operationId'>;
/** Local SQLite owns bounded receipts. Memory only coordinates currently executing calls. */
export class NativeOperations {
  private active=new Map<string,{requestHash:string;work:Promise<Agent.Operation>;claimed:Promise<Agent.Operation>}>();
  constructor(readonly state:Pick<LocalAgentState,'serviceNodeId'|'read'|'write'>,readonly memory:NativeJournal) {}
  private key({callerPrincipalId,operationId}:Identity):string {return hashJson({callerPrincipalId,operationId});}
  requestId(identity:Identity):string {return this.key(identity).slice(7,39);}
  async settle(identity:Identity):Promise<void> {
    await this.active.get(this.key(identity))?.work.catch(()=>undefined);
    await this.read(identity);
  }
  async read(identity:Identity):Promise<Agent.Operation|null> {
    const saved=await this.state.read<Agent.Operation>('operation',this.key(identity));if(!saved)return null;
    validateAgent('Operation',saved.value);
    requireThat(saved.value.callerPrincipalId===identity.callerPrincipalId&&saved.value.operationId===identity.operationId&&saved.value.serviceNodeId===this.state.serviceNodeId,'target_conflict','Receipt identity differs from its Hive key.');
    const live=this.memory.get(identity);
    if(live && live.requestHash===saved.value.requestHash) {
      if(!this.active.has(this.key(identity))&&['succeeded','failed','outcome_unknown'].includes(live.phase)) {
        if(hashJson(live)!==hashJson(saved.value)) await this.state.write('operation',this.key(identity),live,saved.pin);
        this.memory.retainedInHive(identity);
      }
      return live;
    }
    if(['accepted','dispatched'].includes(saved.value.phase)) {
      return {...saved.value,phase:'outcome_unknown',code:'native_connection_lost'};
    }
    return saved.value;
  }
  run(identity:Identity,method:string,params:Wire.Json,action:()=>Promise<Agent.Operation>,prevent=false):Promise<Agent.Operation> {
    const key=this.key(identity),requestHash=hashJson({method,params}),prior=this.active.get(key);
    if(prior){requireThat(prior.requestHash===requestHash,'mutation_conflict','Operation has different arguments.');return prior.claimed.then(async value=>await this.read(identity)??value);}
    let claimedResolve!:(value:Agent.Operation)=>void,claimedReject!:(error:unknown)=>void;
    const claimed=new Promise<Agent.Operation>((resolve,reject)=>{claimedResolve=resolve;claimedReject=reject;});void claimed.catch(()=>undefined);
    const work=(async()=>{
      const existing=await this.read(identity);
      if(existing){requireThat(existing.requestHash===requestHash,'mutation_conflict','Operation has different original arguments.');claimedResolve(existing);return existing;}
      const accepted=prevent?this.memory.prevent(identity,method,params):this.memory.accept(identity,method,params).operation;
      const intent = prevent ? accepted : {...accepted,phase:'dispatched' as const,epoch:this.memory.epoch,
        requestId:method==='agent.answer'?(params as unknown as Agent.AnswerInput).identity.requestId:this.requestId(identity)};
      validateAgent('Operation',intent);
      let saved;
      try { saved=await this.state.write('operation',key,intent); }
      catch(error) {
        // A concurrent owner may have claimed this key. A failed/uncertain local write
        // never authorizes sending the native request.
        this.memory.forget(identity);
        if(error instanceof IvyError&&['mutation_conflict','revision_conflict','path_conflict','name_conflict','already_exists'].includes(error.code)) {
          const found=await this.read(identity); if(found){requireThat(found.requestHash===requestHash,'mutation_conflict','Operation has different arguments.');return found;}
        }
        throw error;
      }
      claimedResolve(accepted);
      if(prevent){this.memory.retainedInHive(identity);return accepted;}
      let result:Agent.Operation;
      try { result=await action(); }
      catch(error) {
        const observed=this.memory.get(identity);
        if(observed?.phase==='accepted') result=this.memory.rejectBeforeDispatch(identity,IvyError.from(error).code);
        else if(observed&&['succeeded','failed','outcome_unknown'].includes(observed.phase)) result=observed;
        else throw error; // A dispatched effect without a terminal receipt remains uncertain.
      }
      await this.state.write('operation',key,result,saved.pin);
      if(['succeeded','failed','outcome_unknown'].includes(result.phase))this.memory.retainedInHive(identity);
      return result;
    })();
    this.active.set(key,{requestHash,work,claimed});void work.then(claimedResolve,claimedReject).finally(()=>this.active.delete(key)).catch(()=>undefined);return work;
  }
}
