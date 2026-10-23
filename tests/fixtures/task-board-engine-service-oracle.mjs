import{readFileSync,existsSync,readdirSync}from'node:fs';import{registerHooks,stripTypeScriptTypes}from'node:module';import{fileURLToPath}from'node:url';import assert from'node:assert/strict';
import {coordination} from './task-board-coordination-oracle.mjs';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const{canonical,hashJson}=await import('../../packages/contracts/src/canonical.ts'),{IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts'),{validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts'),{checkedNativeContract}=await import('../../packages/contracts/src/checked-native-contract.ts'),{validateNativePlanDraft,saveNativePlan}=await import('../../packages/sdk/src/native-plan.ts'),{BoundToolClient}=await import('../../packages/sdk/src/client.ts');
const at='2026-01-02T03:04:05.006Z',FixedDate=class extends Date{constructor(){super(at);}},pin=(objectId,revision=1)=>({objectId,revision}),mutation=(scope,step)=>'task-board:'+hashJson({scope,step}).slice(7);
const engineSource=readFileSync('services/task-board/src/runtime/engine.ts','utf8'),opsSource=readFileSync('services/task-board/src/runtime/operations.ts','utf8'),coordSource=readFileSync('services/task-board/src/runtime/coordination.ts','utf8'),configSource=readFileSync('services/task-board/src/runtime/configuration.ts','utf8'),checksSource=readFileSync('services/task-board/src/runtime/checks.ts','utf8'),ownerSource=readFileSync('packages/sdk/src/connection-owner.ts','utf8');
const connectionOwner=new Function('ServiceConnection',stripTypeScriptTypes(ownerSource.replace(/^import .*;\r?\n/gm,'').replace('export async function','async function'))+'\nreturn connectionOwner;')(class ServiceConnection{});
const Operations=new Function('canonical','hashJson','IvyError','requireThat','identityHash','mutation','contractVersion','Date',stripTypeScriptTypes(opsSource.replace(/^import [\s\S]*?;\r?\n/gm,'').replace('export class','class'),{mode:'transform'})+'\nreturn TaskBoardOperations;')(canonical,hashJson,IvyError,requireThat,(callerPrincipalId,operationId)=>hashJson({callerPrincipalId,operationId}).slice(7),mutation,()=> '1.0.0',FixedDate);
const Coordination=coordination(coordSource,{canonical,hashJson,IvyError,requireThat,mutation,Date:FixedDate});
const configurationView=new Function(stripTypeScriptTypes(configSource.slice(configSource.indexOf('export const configurationName'),configSource.indexOf('export async function checkConfiguration')).replaceAll('export ',''))+'\nreturn configurationView;')();
const planSemantics=new Function('requireThat',stripTypeScriptTypes(checksSource.slice(checksSource.indexOf('export function planSemantics'),checksSource.indexOf('export class TaskBoardChecks')).replace('export ',''))+'\nreturn planSemantics;')(requireThat);
let validate;
const Subject=new Function('validateTaskBoard','requireThat','TaskBoardOperations','TaskBoardCoordination','TaskBoardChecks','TaskBoardCategoryProjection','connectionOwner','checkedNativeContract','validateNativePlanDraft','planSemantics','BoundToolClient','saveNativePlan','hashJson','configurationView','contractVersion',stripTypeScriptTypes(engineSource.slice(engineSource.indexOf('export class TaskBoardEngine'),engineSource.indexOf('  async operation(')).concat(engineSource.slice(engineSource.indexOf('  async operation('),engineSource.indexOf('  private async normalizedCategory('))).replace('export class','class')+'async execute(operation){return this.store.effect("execute",[operation.pin,operation.value],()=>({archive:this.store.archiveOutcome,operationId:operation.value.operationId,actor:operation.value.actor,request:operation.value.request}));}}',{mode:'transform'})+'\nreturn TaskBoardEngine;')((...a)=>validate(...a),requireThat,Operations,Coordination,class{},class{},connectionOwner,checkedNativeContract,validateNativePlanDraft,planSemantics,BoundToolClient,saveNativePlan,hashJson,configurationView,()=> '1.8.0');
const settings=(root='root')=>({principalId:'board',rootObjectId:root,scheduler:{enabled:true,intervalMs:1000,pageSize:50},phoneTarget:null}),owner={serviceNodeId:'task-node',generation:7},request=(patch={})=>({action:'cancel',operationId:'op',taskId:'task',expectedRevision:1,reason:'retained',...patch}),invoke={method:'invoke',request:request()},record=(id,key,value,name=id)=>({key,name,pin:pin(id),value,metadata:{parentId:'root'},revision:{contentHash:hashJson(value)}}),operation=phase=>record('retained','task-board/operation',{schemaVersion:1,operationId:'prior',callerPrincipalId:'caller',actor:{principalId:'caller',serviceNodeId:'task-node',generation:7,source:'user'},request:request(),createdAt:at,updatedAt:at,phase,writes:[],outcome:null,error:null}),gate=()=>record('gate','task-board/coordination',{schemaVersion:1,principalId:'board',activeOperation:pin('retained'),updatedAt:at},'TaskBoard publication gate task');

const main=readFileSync('services/task-board/src/main.ts','utf8');
const handlers=new Function('taskBoardTools','requireThat','Date','engine','runtime','settings','kick',stripTypeScriptTypes(main.slice(main.indexOf('  const handlers:'),main.indexOf('  const service =')),{mode:'transform'})+'return handlers;');
const specs=[],add=(name,tool,args={},options={})=>specs.push({name,steps:[{tool,request:args}],options});
for(const recovered of [false,true])add('workspace recovered '+recovered,'workspace',{}, {recovered});
add('workspace null root UTF16 caller','workspace',{}, {root:null,caller:'caller\ud800🧪'});
add('workspace wrong generation','workspace',{}, {generation:8});
add('workspace aborted after owner','workspace',{}, {abort:true});
add('workspace wrong owner','workspace',{}, {ownerPatch:{principalId:'other'}});
add('configuration defaults','configuration');
add('configuration retained','configuration',{}, {records:[record('config','task-board/configuration',{schemaVersion:1,defaults:{retained:true}},'TaskBoard configuration')]});
add('categories','categories');add('category provider failure','categories',{}, {fault:{where:'categories',code:'service_unavailable',outcome:'unknown',remaining:1}});
for(const action of ['start','continue']){add('recovery blocks '+action,action,request({action}),{recovered:false});add('recovery allows '+action,action,request({action}));}
add('unrecovered cancellation admitted','cancel',request(),{recovered:false});
add('ordinary invoke','cancel',request());add('aborted invoke before owner','cancel',request(),{abort:true});
add('UTF16 invoke','cancel',request({operationId:'op\udfff'}),{caller:'caller\ud800'});
add('outer operation mismatch','cancel',request(),{outerOperationId:'other'});
add('worker agent comment','comment',request({action:'comment',authorKind:'agent'}));
add('worker child creation','create',request({action:'create',parentTaskId:'parent'}));
add('invoke provider failure','cancel',request(),{fault:{where:'execute',code:'deadline_exceeded',outcome:'unknown',remaining:1}});
const archive={action:'archive',operationId:'archive-op',taskId:'00000000-0000-4000-8000-000000000001',expectedRevision:1,archived:true};
add('archive returns metadata','archive',archive);add('archive restore','archive',{...archive,archived:false});
add('archive missing metadata refuses','archive',archive,{archiveOutcome:null});
add('archive invalid request before owner','archive',{...archive,archived:'yes'});
add('archive outer identity mismatch','archive',archive,{outerOperationId:'other'});
add('archive aborted after validation owner','archive',archive,{abort:true});
add('archive exact workspace','archive',{...archive,expectedWorkspace:{principalId:'board',rootObjectId:'root',callerPrincipalId:'caller'}});
for(const key of ['principalId','rootObjectId','callerPrincipalId'])add('archive changed '+key,'archive',{...archive,expectedWorkspace:{principalId:'board',rootObjectId:'root',callerPrincipalId:'caller',[key]:'other'}});
add('operation String coercion','operation',{operationId:17});
add('read passes full query','read',{task:'00000000-0000-4000-8000-000000000001'});
add('list passes full query','list',{parentTaskId:'parent',limit:13});
add('marker validates before owner','markCommentsRead',{});
const results=[];
for(const spec of specs){const opts=structuredClone(spec.options),boundaries=[],docs=new Map((opts.records??[]).map(d=>[d.pin.objectId,structuredClone(d)])),root=opts.root===undefined?'root':opts.root;
 const error=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome,details:e.details??null};};
 const effect=(kind,args,fn)=>{const frame={kind,args:structuredClone(args)};boundaries.push(frame);const fault=opts.fault?.where===kind&&(!opts.fault.method||args[0]===opts.fault.method)&&opts.fault.remaining-->0?opts.fault:null;try{if(fault&&!fault.after)throw new IvyError(fault.code,'Injected '+kind,fault.outcome??'not_executed',{boundary:kind});const value=fn();if(fault)throw new IvyError(fault.code,'Injected '+kind,fault.outcome??'not_executed',{boundary:kind});frame.value=structuredClone(value??null);return value;}catch(e){frame.error=error(e);throw e;}};
 validate=(name,value)=>effect('validate',[name,value],()=>validateTaskBoard(name,value));
 const store={rootObjectId:root,effect,archiveOutcome:opts.archiveOutcome===undefined?{archived:true,retained:true}:opts.archiveOutcome,client:{async request(method,args){const normalized=structuredClone(args);if(method==='tools.call')assert.equal('operationId'in args,false,'Project reads retain the original absence of an operation identity.');return effect('rpc',[method,normalized],()=>{if(method==='serviceNodes.get')return args.serviceNodeId==='task-node'?{principalId:'board',serviceName:'task-board',connected:true,synced:true,...opts.ownerPatch}:{serviceName:'agent-manager',hostId:'host',...opts.targetPatch};if(method==='system.status')return{runtimeEpoch:'11111111-1111-4111-8111-111111111111'};if(method==='tools.list')return{provider:{node:{serviceNodeId:opts.provider??args.serviceNodeId}},items:opts.missingTool?[]:[{qualifiedName:'agent.resolveProject',definition:{namespace:'agent',name:'resolveProject',interfaceVersion:opts.interfaceVersion??'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null};if(method==='tools.call')return{cwd:opts.observedCwd??'/workspace'};throw Error('Unexpected RPC '+method);});}},retiredOperation(c,id){return effect('retired',[c,id],()=>!!opts.retired);},pendingCoordination(){return effect('pending',[],()=>opts.pending??null);},operationGates(id){return effect('gateNames',[id],()=>opts.gateNames??[]);},retainOperationGates(id,names){effect('retainGates',[id,names],()=>{opts.gateNames=names;});},async page(key,options){return effect('page',[key,options],()=>({items:[],nextCursor:null}));},async named(key,name,parent){return effect('named',[key,name,parent??null],()=>[...docs.values()].find(d=>d.key===key&&d.name===name&&d.metadata.parentId===(parent??root))??null);},async read(key,address,primary=true){return effect('read',[key,address,primary],()=>{const d=docs.get(typeof address==='string'?address:address.objectId);requireThat(d,'not_found','Missing fixture document.');return structuredClone(d);});},async write(key,value,m,destination){return effect('write',[key,value,m,destination],()=>{const old=destination.create?null:docs.get(destination.objectId),id=old?.pin.objectId??'doc'+(docs.size+1),p=pin(id,(old?.pin.revision??0)+1);docs.set(id,{key,name:old?.name??destination.create.name,pin:p,value:structuredClone(value),metadata:{parentId:old?.metadata.parentId??destination.create.parentId},revision:{contentHash:hashJson(value)}});return p;});}};
 const selected={...settings(root),...opts.settingsPatch,...(opts.settingsRoot===undefined?{}:{rootObjectId:opts.settingsRoot})},outcomes=[];let engine;
 try{engine=new Subject(store,selected,owner);}catch(e){outcomes.push({error:error(e)});}

 if(engine){
  engine.categories=async()=>effect('categories',[],()=>({categories:['Ivy','Work']}));
  const runtime={recovered:opts.recovered??true,taskChanged(){effect('taskChanged',[],()=>null);}},kick=()=>effect('kick',[],()=>null);
  const bound=handlers(spec.steps.map(s=>({name:s.tool})),requireThat,FixedDate,engine,runtime,selected,kick);
  for(const step of spec.steps){try{
   const context={callerPrincipalId:opts.caller??'caller',operationId:opts.outerOperationId??step.request?.operationId??'context',generation:opts.generation??7,signal:{throwIfAborted(){if(opts.abort)throw new DOMException('This operation was aborted','AbortError');}}};
   outcomes.push({ok:await bound['task-board.'+step.tool](step.request,context)});
  }catch(e){outcomes.push({error:error(e)});}}
 }
 const expected={outcomes};results.push({...spec,settings:selected,root,boundaries,expected,expectedHash:hashJson(expected)});
}

const find=n=>results.find(r=>r.name===n),code=(n,c)=>assert.equal(find(n).expected.outcomes[0].error?.code,c,n);
for(const n of ['workspace recovered false','workspace recovered true','workspace null root UTF16 caller','configuration defaults','configuration retained','categories','ordinary invoke','UTF16 invoke','worker agent comment','worker child creation','unrecovered cancellation admitted','recovery allows start','recovery allows continue','archive returns metadata','archive restore','archive exact workspace','list passes full query'])assert.ok('ok'in find(n).expected.outcomes[0],n+JSON.stringify(find(n)));
code('read passes full query','not_found');
code('workspace wrong generation','service_not_ready');code('workspace aborted after owner','internal_error');code('aborted invoke before owner','internal_error');code('recovery blocks start','task_board_recovery_pending');code('recovery blocks continue','task_board_recovery_pending');code('archive missing metadata refuses','task_board_contract_mismatch');code('archive outer identity mismatch','task_board_identity_conflict');
assert.equal(results.length,36);process.stdout.write(JSON.stringify(results));
