import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {readFileSync, existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const path = new URL(specifier.slice(0,-3)+'.ts', context.parentURL);
    if (existsSync(fileURLToPath(path))) return next(path.href,context);
  }
  return next(specifier,context);
}});
const {canonical,hashJson} = await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat} = await import('../../packages/contracts/src/errors.ts');
const storeSource = readFileSync(new URL('../../services/task-board/src/runtime/store.ts',import.meta.url),'utf8');
const helpers = storeSource.slice(storeSource.indexOf('export const identityHash'),storeSource.indexOf('export const taskAttachmentPins')).replaceAll('export ','');
const {identityHash,mutation} = new Function('hashJson',stripTypeScriptTypes(helpers)+'\nreturn {identityHash,mutation};')(hashJson);
const source = readFileSync(new URL('../../services/task-board/src/runtime/operations.ts',import.meta.url),'utf8').replace(/^import [\s\S]*?;\r?\n/gm,'').replace('export class','class');
const FixedDate = class extends Date { constructor() { super('2026-01-02T03:04:05.000Z'); } };
const Operations = new Function('canonical','hashJson','IvyError','requireThat','identityHash','mutation','contractVersion','Date',
  stripTypeScriptTypes(source,{mode:'transform'})+'\nreturn TaskBoardOperations;')(canonical,hashJson,IvyError,requireThat,identityHash,mutation,()=> '1.0.0',FixedDate);
class Memory {
  rootObjectId = 'root'; docs = new Map(); trace = []; fault = {}; receipts = new Map(); effects = 0;
  async named(key,name,parent) { this.trace.push(['named',key,name,parent??null]); return structuredClone([...this.docs.values()].find(d=>d.key===key&&d.name===name&&d.metadata.parentId===(parent??this.rootObjectId))??null); }
  retiredOperation(caller,id) { this.trace.push(['retired',caller,id]); return !!this.fault.retired; }
  localKey(id) { this.trace.push(['localKey',id]); return this.fault.localKey??null; }
  completeLocalWorkflow(id,at) { this.trace.push(['complete',id,at]); }
  async read(key,address,upgrade=true) { this.trace.push(['read',key,address,upgrade]); if(typeof address!=='string'&&this.fault.readConflict){this.fault.readConflict=false;throw new IvyError('revision_conflict','Fixture read conflict.');} const d=this.docs.get(typeof address==='string'?address:address.objectId);if(!d)throw new IvyError('not_found','Missing fixture document.');return structuredClone(d); }
  async write(key,value,mutation,destination) {
    this.trace.push(['write',key,value,mutation,destination]);
    if (!destination.create && (this.fault.conflicts??0)>0) { this.fault.conflicts--;throw new IvyError('revision_conflict','Fixture CAS conflict.'); }
    const old=destination.create?null:this.docs.get(destination.objectId),id=old?.pin.objectId??'doc'+(this.docs.size+1);
    const pin={objectId:id,revision:(old?.pin.revision??0)+1};
    this.docs.set(id,{key,name:old?.name??destination.create.name,pin,value:structuredClone(value),metadata:{parentId:old?.metadata.parentId??destination.create.parentId},revision:{contentHash:hashJson(value)}});
    if (destination.create && this.fault.createConflict) { this.fault.createConflict=false;throw new IvyError('mutation_conflict','Fixture lost create reply.'); }
    return structuredClone(pin);
  }
  async preparedMutationId(mutation) { this.trace.push(['prepared',mutation]);return 'prepared:'+mutation; }
  async writePrepared(key,value,mutation,destination) {
    this.trace.push(['apply',key,value,mutation,destination]);
    if (!this.receipts.has(mutation)) { this.effects++;this.receipts.set(mutation,{objectId:'destination',revision:2}); }
    if (this.fault.unknown) { this.fault.unknown=false;throw new IvyError('outcome_unknown','Fixture lost external reply.','unknown'); }
    return structuredClone(this.receipts.get(mutation));
  }
}
const begin={method:'begin',args:[{action:'edit',operationId:'epoch:1:id'}, {principalId:'caller',source:'worker'}]};
const stage={method:'stage',args:['doc1',0,'task-board/task',{objectId:'destination',expectedRevision:1},{title:'original'}]};
const apply={method:'apply',args:['doc1',0]};
const finish={method:'finish',args:['doc1',{operationId:'epoch:1:id',task:null}]};
const native={key:'restore',operationId:'native:1:id',observation:null,params:{name:'original'}};
const scenarios=[
 ['begin replay',[begin,begin]],
 ['begin conflicting request',[begin,{method:'begin',args:[{action:'archive',operationId:'epoch:1:id'},begin.args[1]]}]],
 ['expired identity',[{fault:{retired:true}},begin]],
  ['lost create reply',[{fault:{createConflict:true}},begin]],
 ['create read recovery',[{fault:{readConflict:true}},begin]],
 ['write and finish replay',[begin,stage,apply,apply,finish,finish]],
 ['stage keeps first bytes',[begin,stage,{method:'stage',args:[...stage.args.slice(0,4),{title:'replacement'}]},apply]],
 ['stage conflicting destination',[begin,stage,{method:'stage',args:['doc1',0,'task-board/task',{objectId:'different',expectedRevision:1},{}]}]],
 ['out of order',[begin,{method:'stage',args:['doc1',1,...stage.args.slice(2)]}]],
 ['missing apply',[begin,apply]],
 ['pending finish',[begin,stage,finish]],
 ['wrong outcome identity',[begin,{method:'finish',args:['doc1',{operationId:'wrong'}]}]],
 ['conflicting completed outcome',[begin,finish,{method:'finish',args:['doc1',{operationId:'epoch:1:id',task:'different'}]}]],
 ['lost external reply',[begin,stage,{fault:{unknown:true}},apply,apply,finish]],
  ['one CAS retry',[begin,{fault:{conflicts:1}},stage]],
 ['amend read recovery',[begin,{fault:{readConflict:true}},stage]],
 ['CAS exhaustion',[begin,{fault:{conflicts:12}},stage]],
  ['payload create replay',[begin,{fault:{createConflict:true}},stage]],
 ['payload ownership',[begin,stage,{patch:{id:'doc2',parentId:'other'}},apply]],
 ['payload hash',[begin,stage,{patch:{id:'doc2',contentHash:'wrong'}},apply]],
 ['finished stage append',[begin,finish,stage]],
 ['archive replay',[begin,{method:'archiveIdentity',args:['doc1']},{method:'archiveIdentity',args:['doc1']}]],
 ['native observation replay',[begin,{method:'prepareNative',args:['doc1',native]},{method:'observeNative',args:['doc1','restore',{operationId:'native:1:id',phase:'succeeded'}]},{method:'prepareNative',args:['doc1',native]}]],
 ['native intent conflict',[begin,{method:'prepareNative',args:['doc1',native]},{method:'prepareNative',args:['doc1',{...native,params:{name:'other'}}]}]],
 ['native observation identity',[begin,{method:'prepareNative',args:['doc1',native]},{method:'observeNative',args:['doc1','restore',{operationId:'wrong'}]}]],
 ['native missing call',[begin,{method:'observeNative',args:['doc1','restore',{operationId:'native:1:id'}]}]],
 ['finished native work',[begin,finish,{method:'prepareNative',args:['doc1',native]}]],
 ['needs attention then failure',[begin,{method:'failure',args:['doc1',{code:'fixture_failed',message:'Failed',outcome:'unknown'},true]},stage,{method:'failure',args:['doc1',{code:'fixture_failed',message:'Final',outcome:'not_executed'},false]},apply]],
 ['failure after success',[begin,finish,{method:'failure',args:['doc1',{code:'ignored',message:'ignored',outcome:'unknown'},false]}]],
 ['scheduler cleanup',[{method:'begin',args:[{action:'continue',operationId:'epoch:1:id',intent:{objectId:'plan',revision:1}},{principalId:'caller',source:'scheduler'}]},{fault:{localKey:'task-board/native-plan'}},{method:'failure',args:['doc1',{code:'failed',message:'Failed',outcome:'not_executed'},false]}]],
 ['UTF-16 caller identity',[{method:'begin',args:[{action:'edit',operationId:'epoch:1:id\ud800'},{principalId:'caller\udfff',source:'worker'}]}]],
];
for (const index of [-1,0.5,16,9007199254740992]) scenarios.push(['stage bound '+index,[begin,{method:'stage',args:['doc1',index,...stage.args.slice(2)]}]]);
const results=[];
for (const [name,steps] of scenarios) {
  const store=new Memory(), operations=new Operations(store), outcomes=[];
  for(const step of steps) {
    if(step.fault){Object.assign(store.fault,step.fault);continue;}
    if(step.patch){const doc=store.docs.get(step.patch.id);if('parentId'in step.patch)doc.metadata.parentId=step.patch.parentId;if('contentHash'in step.patch)doc.revision.contentHash=step.patch.contentHash;continue;}
    const args=structuredClone(step.args);
    if(step.method==='failure')args[1]=new IvyError(args[1].code,args[1].message,args[1].outcome);
    try{const result=await operations[step.method](...args);outcomes.push(result?.pin&&result?.value?{pin:result.pin,value:result.value}:result);}
    catch(error){outcomes.push({error:{code:error.code,message:error.message,outcome:error.outcome}});}
  }
  results.push({name,steps,expected:{outcomes,trace:store.trace,effects:store.effects}});
}
process.stdout.write(JSON.stringify(results));
