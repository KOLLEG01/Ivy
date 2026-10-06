import assert from 'node:assert/strict';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { deflateSync } from 'node:zlib';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { HiveClient, discover, callBound } from '../../../dist/packages/sdk/src/client.js';
import { canonical, digest, hashJson } from '../../../dist/packages/contracts/src/canonical.js';
import { validateAgent } from '../../../dist/packages/contracts/src/validation.js';
import { nativeFrameBytes, nativeRequestFrameBytes, nativeAnswerFrameBytes, managementFrameBytes } from '../../../dist/services/agent-manager/src/limits.js';

// Actual native image decoding through a selected isolated owner. Synthetic PNGs have valid
// ancillary padding, exact two-MiB file sizes and undisclosed solid fill colors. No outside action.
const {values}=parseArgs({options:Object.fromEntries(['config','owner','project','evidence','model','effort'].map(key=>[key,{type:'string'}]))});
for(const key of ['config','owner','project','evidence','model','effort'])assert.ok(values[key],'Missing --'+key);
const config=JSON.parse(await readFile(resolve(values.config),'utf8'));
assert.ok(config.hostId.endsWith('-acceptance'));
assert.ok(config.instances.some(i=>i.componentId==='agent-manager'&&i.serviceNodeId===values.owner));
const credential=config.instances.find(i=>i.componentId==='hive').settings.credentials[0].token;
const client=new HiveClient(config.publicBaseUrl,{credential});
const root=resolve(values.evidence), reportPath=join(root,'report.json');await mkdir(root,{recursive:true});
const id=randomUUID(), marker='IVY-IMAGE-'+id;
const report={schemaVersion:1,id,marker,owner:values.owner,project:values.project,startedAt:new Date().toISOString(),phase:'starting',operations:[]};
await writeFile(reportPath,'{}\n',{flag:'wx'});
const save=async(phase,fields={})=>{Object.assign(report,fields,{phase,observedAt:new Date().toISOString()});await writeFile(reportPath,JSON.stringify(report,null,2)+'\n');};
const call=async(name,args,operationId)=>callBound(client,await discover(client,name,{serviceNodeId:values.owner}),args,operationId,{timeoutMs:65000});
const native=async(method,params,label)=>{
 const operationId=id+'-'+label; report.operations.push({method,operationId,requestHash:hashJson({method,params})});await save('calling-'+label);
 return call('codex.'+method,params,operationId);
};
function crc32(bytes){let value=0xffffffff;for(const byte of bytes){value^=byte;for(let bit=0;bit<8;bit++)value=value&1?0xedb88320^(value>>>1):value>>>1;}return (value^0xffffffff)>>>0;}
function chunk(name,data){const type=Buffer.from(name), body=Buffer.concat([type,data]), size=Buffer.alloc(4), crc=Buffer.alloc(4);size.writeUInt32BE(data.length);crc.writeUInt32BE(crc32(body));return Buffer.concat([size,body,crc]);}
function png(color){
 const header=Buffer.alloc(13);header.writeUInt32BE(64,0);header.writeUInt32BE(64,4);header[8]=8;header[9]=2;
 const pixels=Buffer.alloc(64*(1+64*3));for(let y=0;y<64;y++)for(let x=0;x<64;x++)for(let c=0;c<3;c++)pixels[y*193+1+x*3+c]=color[c];
 const prefix=Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]),chunk('IHDR',header),chunk('IDAT',deflateSync(pixels))]);
 const end=chunk('IEND',Buffer.alloc(0)), length=2097152-prefix.length-end.length-12, padding=Buffer.alloc(length,97);
 Buffer.from('FixturePadding\0').copy(padding);return Buffer.concat([prefix,chunk('tEXt',padding),end]);
}
try{
 const before=await call('agent.status',{});validateAgent('Status',before);assert.equal(before.state,'ready');
 const capacity=await call('agent.frameLimits',{});validateAgent('FrameLimits',capacity);
 for(const key of ['serviceNodeId','epoch','nativeVersion','nativeExecutableHash','catalogHash'])assert.equal(capacity[key],before[key]);
 assert.equal(capacity.requestFrameBytes,nativeRequestFrameBytes);assert.equal(capacity.receivedFrameBytes,nativeFrameBytes);
 assert.equal(capacity.answerFrameBytes,nativeAnswerFrameBytes);assert.equal(capacity.managementFrameBytes,managementFrameBytes);
 const projects=await call('agent.projects',{});assert.ok(projects.projects.some(p=>p.paths.includes(values.project)));
 const images=[png([255,0,0]),png([0,0,255])];
 for(let index=0;index<images.length;index++){assert.equal(images[index].length,2097152);await writeFile(join(root,'image-'+(index+1)+'.png'),images[index],{flag:'wx'});}
 await save('native-owner-verified',{before,capacity,images:images.map(bytes=>({bytes:bytes.length,contentHash:digest(bytes)}))});
 let cursor, model; const cursors=new Set();
 do {
  const models=await native('model/list',cursor?{cursor}:{},'models-'+cursors.size);
  model=models.data.find(m=>m.model===values.model&&!m.hidden);
  cursor=models.nextCursor;
  if(cursor){assert.ok(!cursors.has(cursor),'Repeated model-list cursor');cursors.add(cursor);assert.ok(cursors.size<=20,'Model-list page bound exceeded');}
 } while(!model&&cursor);
 assert.ok(model,'Requested image-test model is unavailable');
 assert.ok(model.supportedReasoningEfforts.some(e=>e.reasoningEffort===values.effort),'Requested reasoning effort is unavailable');
 const thread=await native('thread/start',{cwd:values.project,model:model.model,allowProviderModelFallback:false},'thread');assert.ok(thread.thread.id);
 await save('thread-created',{threadId:thread.thread.id,model:model.model,effort:values.effort});
 const input=[{type:'text',text:`This is an isolated image interpretation test. Identify the solid fill color of image 1 and image 2 from the attached images, in that order. Reply only "${marker}: first=<color>; second=<color>" using common English color names. Do not execute tools, edit files, send messages, make calls or take any other action.`},
  ...images.map(bytes=>({type:'image',url:'data:image/png;base64,'+bytes.toString('base64')}))];
 const params={threadId:report.threadId,input,model:model.model,effort:values.effort};
 const requestBytes=Buffer.byteLength(canonical({id:'00000000-0000-0000-0000-000000000000',method:'turn/start',params}));
 assert.ok(requestBytes>nativeAnswerFrameBytes&&requestBytes<nativeRequestFrameBytes);await save('turn-intent',{requestBytes});
 const started=await native('turn/start',params,'turn');assert.ok(started.turn.id);await save('turn-started',{turnId:started.turn.id});
 const deadline=Date.now()+240000;let terminal;
 while(Date.now()<deadline){
  const observation=await call('agent.read',{nativeVersion:before.nativeVersion,method:'thread/read',params:{threadId:report.threadId,includeTurns:true}});
  validateAgent('ReadObservation',observation);assert.equal(observation.epoch,before.epoch);assert.ok('result' in observation.reply);
  const turn=observation.reply.result.thread.turns.find(t=>t.id===report.turnId);
  if(turn&&['completed','failed','interrupted'].includes(turn.status)){terminal={turn,observation};break;}await delay(1000);
 }
 assert.ok(terminal,'Native image turn did not reach a terminal state.');await writeFile(join(root,'terminal-observation.json'),canonical(terminal.observation)+'\n',{flag:'wx'});
 const output=terminal.turn.items.filter(item=>item.type==='agentMessage').map(item=>item.text).join('\n');
 await save('terminal-observed',{nativeState:terminal.turn.status,output,observationHash:hashJson(terminal.observation)});
 assert.equal(terminal.turn.status,'completed');assert.ok(output.includes(marker));assert.match(output,/first\s*=\s*red/i);assert.match(output,/second\s*=\s*blue/i);
 const receipt=await call('agent.operation',{operationId:id+'-turn'});validateAgent('Operation',receipt);
 assert.equal(receipt.phase,'succeeded');assert.equal(receipt.requestHash,hashJson({method:'turn/start',params}));
 assert.equal(receipt.epoch,before.epoch);assert.equal(receipt.reply.result.turn.id,report.turnId);
 const after=await call('agent.status',{});assert.equal(after.epoch,before.epoch);assert.equal(after.state,'ready');
 await save('passed',{after,receiptHash:hashJson(receipt),finishedAt:new Date().toISOString()});console.log(JSON.stringify({phase:report.phase,root,threadId:report.threadId,turnId:report.turnId,requestBytes}));
}catch(error){await save('failed',{failure:{code:error.code??null,message:error.message},finishedAt:new Date().toISOString()});throw error;}
