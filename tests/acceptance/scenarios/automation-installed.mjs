import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { parseArgs } from 'node:util';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { HiveClient } from '../../../dist/packages/sdk/src/client.js';
import { ServiceClient } from '../../../dist/packages/sdk/src/service.js';
import { cli } from '../../../dist/packages/cli/src/main.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { validateAutomation } from '../../../dist/packages/contracts/src/validation.js';
import { digest, hashJson } from '../../../dist/packages/contracts/src/canonical.js';

// Runs only on the selected real acceptance host. Uses the ordinary prepared candidate and local
// deployment CLI, a real SDK event publisher, and the independently guarded automation process.
// The selected instance must be new/disabled. Only its lifecycle and synthetic records are changed.
const { values } = parseArgs({ options: Object.fromEntries(['config','instance','candidate-file','candidate-transfer','evidence'].map(key => [key,{type:'string'}])) });
for(const key of ['config','instance','evidence']) assert.ok(values[key], 'Missing --'+key);
assert.notEqual(Boolean(values['candidate-file']),Boolean(values['candidate-transfer']),'Select exactly one preparation receipt or verified transfer report');
const configPath=resolve(values.config), config=JSON.parse(await readFile(configPath,'utf8'));
assert.ok(config.hostId.endsWith('-acceptance'));
const instance=config.instances.find(row=>row.instanceId===values.instance); assert.equal(instance?.componentId,'automation-example');
validateAutomation('Settings',instance.settings); const settings=instance.settings, definition=settings.definition, definitionHash=hashJson(definition);
assert.equal(definition.catchUp,'all'); assert.equal(definition.inputTopic,'automation-input.sample');
assert.equal(definition.localTime,'00:00', 'This acceptance intentionally exercises midnight daily catch-up.');
assert.ok(definition.inputSource.startsWith('service:'+config.hostId+'.automation-input-'));
const sourceNode=definition.inputSource.slice(8), credential=config.instances.find(row=>row.componentId==='hive').settings.credentials[0].token;
const client=new HiveClient(config.publicBaseUrl,{credential}), root=resolve(values.evidence);
await mkdir(root,{recursive:true}); await writeFile(join(root,'report.json'),'{}\n',{flag:'wx'});
const id=randomUUID(), report={schemaVersion:1,id,hostId:config.hostId,instanceId:instance.instanceId,serviceNodeId:instance.serviceNodeId,startedAt:new Date().toISOString(),phase:'preflight',definition,definitionHash,operations:[],events:[],outsideEffects:false};
const save=async(phase, details={})=>{Object.assign(report,details,{phase,observedAt:new Date().toISOString()});
  for(const secret of [credential,instance.credential]) assert.equal(JSON.stringify(report).includes(secret),false);
  await atomicJson(join(root,'report.json'),report); console.log(JSON.stringify({phase,reportPath:join(root,'report.json')}));};
const until=async(check,label,ms=90000)=>{const end=Date.now()+ms;while(Date.now()<end){const result=await check();if(result)return result;await delay(1000);}throw new Error('Acceptance deadline: '+label);};
const hostStatus=async()=>{const value=await cli(['status','--config',configPath,'--json']);assert.equal(value.exitCode,0);return value.output.data;};
const lifecycle=async(action,candidateId)=>{
  const operationId=id+'-'+action+'-'+report.operations.length;
  const original={action,instanceId:instance.instanceId,operationId,...(candidateId?{candidateId}:{})};
  report.operations.push(original);await save('lifecycle-'+action);
  const result=await cli([action,'--config',configPath,'--instance',instance.instanceId,'--operation-id',operationId,...(candidateId?['--candidate',candidateId]:[]),'--json']);
  original.acceptance=result.output; await save('waiting-'+action); assert.equal(result.exitCode,0); assert.ok(result.output.deploymentId);
  const final=await until(async()=>{const value=await cli(['status','--config',configPath,'--deployment',result.output.deploymentId,'--json']);
    return ['succeeded','failed','needs_attention','rolled_back'].includes(value.output.data?.record?.phase)?value:null;},action,630000);
  original.result=final.output;await save('observed-'+action);assert.equal(final.exitCode,0);assert.equal(final.output.code,'deployment_succeeded');
  return (await hostStatus()).observations[instance.instanceId];
};
const documents=async(contract)=>{
  const page=await client.request('objects.query',{contractKey:'automation-example/'+contract,contractVersions:['1.0.0'],includeArchived:true,
    where:{op:'eq',field:'data:/definitionHash',value:definitionHash},limit:100});assert.equal(page.nextCursor,null);
  const docs=await Promise.all(page.items.map(row=>client.request('objects.read',{objectId:row.objectId})));
  for(const doc of docs){assert.equal(doc.object.parentId,definition.rootObjectId);assert.equal(doc.object.effectivelyArchived,false);assert.equal(doc.revision.contractVersion,'1.0.0');
    assert.equal(doc.content.encoding,'json');validateAutomation({checkpoint:'Checkpoint',period:'Period','event-receipt':'EventReceipt'}[contract],doc.content.value);
    assert.equal(doc.revision.contentHash,hashJson(doc.content.value));if(contract!=='checkpoint')assert.equal(doc.revision.revision,1);}
  return docs;
};
const identities=docs=>docs.map(doc=>({objectId:doc.object.id,revision:doc.revision.revision,contentHash:doc.revision.contentHash})).sort((a,b)=>a.objectId.localeCompare(b.objectId));
const publisher=new ServiceClient({publicBaseUrl:config.publicBaseUrl,credential:()=>credential,
  identity:{serviceNodeId:sourceNode,serviceName:'automation-input',hostId:config.hostId,version:'0.1.0',buildId:digest(await readFile(new URL(import.meta.url))),hiveProtocol:1},
  registry:()=>({namespaces:[{namespace:'automation-input',description:'Isolated installed acceptance input',guideMarkdown:'Synthetic bounded events only.',tools:[],inventoryKinds:[],topics:[{topic:definition.inputTopic,version:'1.0.0',title:'Acceptance input',description:'One synthetic bounded acceptance event.',payloadSchema:{type:'object',properties:{value:{type:'integer'}},required:['value'],additionalProperties:false},eventKinds:[]}]}],contracts:[],requiredContracts:[]}),
  handlers:{},reconcile:async()=>undefined,heartbeatMs:2000});
const publish=async(value)=>{const request={topic:definition.inputTopic,topicVersion:'1.0.0',payload:{value},mutationId:id+'-source-'+value};
  const intent={request};report.events.push(intent);await save('publishing-event-'+value);intent.result=await publisher.connection.request('events.publish',request);await save('published-event-'+value);return intent;};
let stopped=false;
try{
  const before=await hostStatus(); assert.equal(before.unfinished.length,0);
  const selected=before.instances.find(row=>row.instanceId===instance.instanceId);assert.equal(selected.enabled,false);assert.equal(selected.installed,null);
  try{const node=await client.request('serviceNodes.get',{serviceNodeId:sourceNode});assert.equal(node.connected,false);}catch(error){if(error.code!=='not_found')throw error;}
  report.before=before;report.hiveBefore=await client.request('system.status',{});
  publisher.start();await publisher.waitReady(); const first=await publish(1);
  await save('awaiting-checked-candidate');
  const candidate=await until(async()=>{let value;try{value=JSON.parse(await readFile(resolve(values['candidate-file']??values['candidate-transfer']),'utf8'));}catch(error){if(error instanceof SyntaxError||error.code==='ENOENT')return null;throw error;}
    if(values['candidate-transfer']){
      assert.equal(value.schemaVersion,1);assert.equal(value.hostId,config.hostId);assert.equal(value.activated,false);
      assert.equal(value.checksumVerified,true);assert.match(value.manifestHash,/^sha256:[a-f0-9]{64}$/);
      assert.equal(value.original.candidateId,value.candidate.candidateId);assert.match(value.candidate.archiveHash,/^sha256:[a-f0-9]{64}$/);
      return value.candidate;
    }
    assert.equal(value.ok,true);assert.equal(value.code,'candidate_prepared');return value.data;},'candidate preparation',3600000);
  assert.equal(candidate.componentId,'automation-example');assert.equal(candidate.platform.os,process.platform);report.candidate=candidate;
  const installed=await lifecycle('deploy',candidate.candidateId);assert.equal(installed.state,'stopped');
  const running=await lifecycle('enable');assert.equal(running.state,'ready');report.firstBoot=running.health;
  const localParts=new Intl.DateTimeFormat('en-CA',{timeZone:definition.timeZone,year:'numeric',month:'2-digit',day:'2-digit'}).formatToParts(new Date());
  const part=name=>localParts.find(row=>row.type===name).value, today=part('year')+'-'+part('month')+'-'+part('day');
  const expected=[];for(let date=definition.firstDate;date<=today;){expected.push(date);assert.ok(expected.length<=7,'Acceptance scope is at most seven deliberate catch-up dates');const next=new Date(date+'T12:00:00Z');next.setUTCDate(next.getUTCDate()+1);date=next.toISOString().slice(0,10);}
  const periods=await until(async()=>{const docs=await documents('period');return docs.length===expected.length?docs:null;},'daily catch-up');
  assert.deepEqual(periods.map(doc=>doc.content.value.periodDate).sort(),expected);
  for(const doc of periods){assert.equal(doc.content.value.snapshot.buildId,report.hiveBefore.buildId);assert.ok(Date.parse(doc.content.value.snapshot.serverTime)>=Date.parse(report.startedAt));}
  await until(async()=>(await documents('event-receipt')).some(doc=>doc.content.value.sequence===first.result.sequence),'pre-start event receipt');
  report.originalPeriods=identities(periods);report.firstReceipts=identities(await documents('event-receipt'));
  const invalidEvent={topic:definition.inputTopic,topicVersion:'1.0.0',payload:{value:'invalid-integer'},mutationId:id+'-invalid-source'};
  await save('invalid-event-intent',{invalidEvent:{mutationId:invalidEvent.mutationId,payloadHash:hashJson(invalidEvent.payload)}});
  await assert.rejects(publisher.connection.request('events.publish',invalidEvent),error=>{
    assert.equal(error.code,'invalid_arguments');assert.equal(error.outcome,'not_executed');return true;
  });
  await delay(settings.pollMs*3);
  assert.deepEqual(identities(await documents('event-receipt')),report.firstReceipts,'Rejected malformed input cannot create an automation receipt');
  assert.deepEqual(identities(await documents('period')),report.originalPeriods,'Rejected input cannot alter daily catch-up effects');
  await save('invalid-event-rejected');
  const disabled=await lifecycle('disable');assert.equal(disabled.state,'stopped');stopped=true;
  const second=await publish(2);await delay(settings.pollMs*3);
  assert.deepEqual(identities(await documents('event-receipt')),report.firstReceipts,'A stopped process cannot consume the outage event');
  const resumed=await lifecycle('enable');stopped=false;assert.equal(resumed.state,'ready');assert.notEqual(resumed.health.bootId,running.health.bootId);
  await until(async()=>(await documents('event-receipt')).length===2,'outage event catch-up');
  const receipts=await documents('event-receipt');
  for(const event of [first,second]){const doc=receipts.find(row=>row.content.value.sequence===event.result.sequence);assert.ok(doc);assert.equal(doc.content.value.sourceMutationId,event.request.mutationId);assert.equal(doc.content.value.payloadHash,hashJson(event.request.payload));assert.equal(doc.content.value.source,definition.inputSource);}
  assert.deepEqual(await publisher.connection.request('events.publish',first.request),first.result,'Original publisher identity keeps its original event');
  report.recoveredReceipts=identities(receipts);
  const restarted=await lifecycle('restart');assert.equal(restarted.state,'ready');assert.notEqual(restarted.health.bootId,resumed.health.bootId);await delay(settings.pollMs*3);
  assert.deepEqual(identities(await documents('period')),report.originalPeriods);assert.deepEqual(identities(await documents('event-receipt')),report.recoveredReceipts);
  const checkpoint=await documents('checkpoint');assert.equal(checkpoint.length,1);assert.equal(checkpoint[0].content.value.pending,null);assert.ok(checkpoint[0].content.value.nextDate>today);
  report.checkpoint={...identities(checkpoint)[0],value:checkpoint[0].content.value};report.after=await hostStatus();
  for(const row of before.instances.filter(row=>row.instanceId!==instance.instanceId)){
    const after=report.after.observations[row.instanceId],original=before.observations[row.instanceId];
    assert.ok(original && after,'Unrelated instance observations must remain available: '+row.instanceId);
    assert.equal(after.state,original.state,'Unrelated instance state changed: '+row.instanceId);
    assert.equal(after.health?.bootId ?? null,original.health?.bootId ?? null,'Unrelated service restarted: '+row.instanceId);
    assert.equal(after.buildId,original.buildId);
  }
  await save('passed',{completedAt:new Date().toISOString(),lastBoot:restarted.health});
}catch(error){await save('failed',{failure:{code:error.code??error.name,message:error.message},instanceIntentionallyStopped:stopped,completedAt:new Date().toISOString()});process.exitCode=1;}
finally{await publisher.stop();}
