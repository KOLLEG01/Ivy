import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,existsSync} from 'node:fs';
import {resolve} from 'node:path';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,itemId,at} from './inbox-fixtures.mjs';
const {AssignmentRunner}=await load('services/secretary/src/assignment-runner.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {IvyError,canonical,hashJson,parseOperationId}=await load('packages/sdk/src/node.js');
const {secretaryNotificationPolicy}=await load('instructions/secretary-notification-policy.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const seed=JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/Execution')).input);
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const target={serviceNodeId:'fixture.agent',hostId:settings.identity.hostId,nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const scratch=process.argv[2];assert.ok(scratch);
const rows=[],secondId='00000000-0000-4000-8000-000000000003';
const settle=()=>new Promise(r=>setImmediate(r));
const until=async f=>{const end=Date.now()+10000;while(!f()){assert.ok(Date.now()<end,'Controlled work did not reach its boundary');await new Promise(r=>setTimeout(r,5));}};
const outcome=async p=>{try{await p;return {value:null};}catch(e){e=IvyError.from(e);return {code:e.code,message:e.message,outcome:e.outcome};}};
async function add(label,c={}){
 const processCase=c.process??false,pid=resolve(scratch,label+'.pid'),release=resolve(scratch,label+'.release');
 let value={...structuredClone(seed),phase:'preflight',serviceNodeId:target.serviceNodeId,nativeTarget:target,preflightOutput:null};
 value.assignmentSnapshot.preflight={executable:process.execPath,args:[resolve('tests/secretary-rust/assignment-preflight-child.mjs'),'gate',pid,release,String(c.exit??0)],timeoutMs:10000};
 value.executionTarget.threadCwd=scratch;
 const terminal={...structuredClone(value),executionId:'terminal',phase:'failed'};
 const initial=structuredClone(value),steps=[],trace=[],issues=new Map([['unrelated','keep'],['execution:'+value.executionId,'old']]);
 const controller=new AbortController();let store,revision=1,queries=0,rowGate=false,completionRead=false,finalWrite=false;
 const gates={owner:{},row:{}};for(const g of Object.values(gates))g.promise=new Promise(r=>g.resolve=r);
 const read=(id,v,rev)=>({object:{id,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/execution'},revision:{revision:rev,contractVersion:contractVersion('secretary/execution'),mediaType:'application/json',contentHash:hashJson(v),byteLength:Buffer.byteLength(canonical(v)),references:store.objectWriteRequest('secretary/execution',v,{object:{objectId:id,revision:rev}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value:structuredClone(v)}});
 const client={async request(method,p,options){
  const copied=structuredClone(p);if(method==='objects.write'){parseOperationId(copied.mutationId);copied.mutationId='<scoped-operation>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});const n=trace.length,step={method};steps.push(step);
  const gate=n===1?'owner':method==='objects.read'&&p.objectId===secondId?'row':null;
  if(gate){step.gate=gate;gates[gate].seen=true;if(gate==='row')rowGate=true;await gates[gate].promise;}
  let result,error;
  if(c.failAt===n)error={code:'fixture_scan_failure',message:'Fixture scan failed.',outcome:'unknown'};
  else if(method==='serviceNodes.get')result=owner;
  else if(method==='objects.query'){
   const slot=queries++%4,firstRound=queries<=4;
   result={items:processCase&&slot===0&&value.phase==='preflight'?[{objectId:itemId,revision},...(firstRound?[{objectId:secondId,revision:1}]:[])]:slot===3&&value.phase==='preflight'&&processCase?[{objectId:itemId,revision}]:[],nextCursor:null};
  }else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='objects.write'){
   const completion=p.content.value.preflightOutput!=='';
   if(completion&&c.writeFailure)error={code:'fixture_completion_write_failure',message:'Completion write failed.',outcome:'unknown'};
   else {assert.equal(p.expectedRevision,revision);value=structuredClone(p.content.value);revision++;result={object:{id:itemId},revision:{revision}};}
   if(completion)finalWrite=true;
  }else if(method==='objects.read'){
   result=p.objectId===secondId?read(secondId,terminal,1):read(itemId,value,revision);
   if(p.objectId===itemId&&finalWrite)completionRead=true;
  }else assert.fail('Unexpected RPC '+method);
  if(error){step.error=error;throw new IvyError(error.code,error.message,error.outcome);}
  step.value=structuredClone(result);return result;
 }};
 store=new SecretaryStore(client,rootId);
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,now:()=>new Date(at),recoveryIssues:issues});
 const runner=new AssignmentRunner(engine),first=runner.tick(),coalesced=runner.tick(),third=runner.tick();
 assert.equal(first,coalesced);assert.equal(first,third);
 void first.catch(()=>{});let tickDone=false,drainDone=false;
 const result=outcome(coalesced).then(v=>{tickDone=true;return v;});
 await until(()=>gates.owner.seen);gates.owner.resolve();
 let checkpoints=[];
 if(processCase){
  await until(()=>existsSync(pid)&&rowGate);
  const drain=runner.drain().then(()=>{drainDone=true;});
  await settle();checkpoints.push({tickDone,drainDone});
  if(c.midScan){
   writeFileSync(release,'release');
   await until(()=>runner.preflightWork.size===0);assert.ok(finalWrite&&(c.writeFailure||completionRead));
   checkpoints.push({tickDone,drainDone});
   gates.row.resolve();
  }else{
   gates.row.resolve();await result;await settle();
   checkpoints.push({tickDone,drainDone});
   // A second scan while the command is active must not treat its durable fence
   // as a lost result or admit the command again.
   await runner.tick();
   if(c.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
   else writeFileSync(release,'release');
  }
  await drain;
 }else{await result;await runner.drain();}
 const results=[await result];
 if(!processCase)results.push(await outcome(runner.tick()));
 assert.equal(runner.preflightWork.size,0);
 if(processCase){const childPid=Number(readFileSync(pid,'utf8'));assert.throws(()=>process.kill(childPid,0),{code:'ESRCH'});}
 store.close();rows.push({input:{label,processCase,midScan:c.midScan??false,cancel:c.cancel??false,writeFailure:c.writeFailure??false,pid,release,initial,steps},expected:{results,checkpoints,trace,issues:[...issues]}});
}
await add('empty');await add('owner-error',{failAt:1});await add('query-error',{failAt:3});
await add('slow-success',{process:true});await add('mid-scan-success',{process:true,midScan:true});
await add('command-failure',{process:true,exit:7});
await add('completion-write-failure',{process:true,midScan:true,writeFailure:true});
await add('cancel-owned-command',{process:true,cancel:true});
console.log(JSON.stringify({rootId,settings,time:Date.parse(at),notificationPolicy:secretaryNotificationPolicy,rows}));
