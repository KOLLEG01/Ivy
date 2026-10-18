import {existsSync, readFileSync} from 'node:fs';
import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n) {
  if(s.startsWith('.') && s.endsWith('.js') && c.parentURL) {
    const u=new URL(s.slice(0,-3)+'.ts',c.parentURL); if(existsSync(fileURLToPath(u))) return n(u.href,c);
  }
  return n(s,c);
}});
const {canonical,digest,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {managementFrameBytes}=await import('../../packages/contracts/src/limits.ts');
const {validateNativeOperation}=await import('../../packages/sdk/src/native-evidence.ts');
const {checkedNativeContract:catalogFor}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const load=(name)=>readFileSync(new URL('../../services/task-board/src/runtime/'+name+'.ts',import.meta.url),'utf8')
  .replace(/^import .*;\r?\n/gm,'').replace(/^export \{.*\} from .*;\r?\n/gm,'').replaceAll('export ','');
const {saveEvidenceChunks,readEvidenceChunks,evidenceMaximumBytes,evidenceChunkBytes}=new Function('canonical','digest','requireThat','IvyError','managementFrameBytes',
  stripTypeScriptTypes(load('evidence-bytes'))+'\nreturn {saveEvidenceChunks,readEvidenceChunks,evidenceMaximumBytes,evidenceChunkBytes};')
  (canonical,digest,requireThat,IvyError,managementFrameBytes);
// Use the original mutation identity algorithm, without loading the engine graph.
const mutationSource=readFileSync(new URL('../../services/task-board/src/runtime/store.ts',import.meta.url),'utf8')
  .match(/export const mutation = [\s\S]*?;/)[0].replace('export ','');
const mutation=new Function('hashJson',stripTypeScriptTypes(mutationSource)+'\nreturn mutation;')(hashJson);
const {validateNativeInvocation}=await import('../../packages/sdk/src/native-plan.ts');
const {readNativeParameters,saveNativeParameters}=await import('../../packages/sdk/src/native-parameters.ts');
const intent=load('native-intent');
const {resolveNativeRequest,retainNativeRequest}=new Function('checkedNativeContract','validateNativeInvocation','readNativeParameters','saveNativeParameters','mutation','validateTaskBoard',stripTypeScriptTypes(intent.slice(intent.indexOf('async function retainNativeRequest')))+'\nreturn {resolveNativeRequest,retainNativeRequest};')(catalogFor,validateNativeInvocation,readNativeParameters,saveNativeParameters,mutation,validateTaskBoard);
const Evidence=new Function('canonical','digest','hashJson','requireThat','validateNativeOperation','validateTaskBoard','checkedNativeContract','resolveNativeRequest',
  'evidenceMaximumBytes','evidenceChunkBytes','saveEvidenceChunks','readEvidenceChunks','mutation',stripTypeScriptTypes(load('evidence'),{mode:'transform'})+'\nreturn TaskBoardEvidence;')
  (canonical,digest,hashJson,requireThat,validateNativeOperation,validateTaskBoard,catalogFor,resolveNativeRequest,evidenceMaximumBytes,evidenceChunkBytes,saveEvidenceChunks,readEvidenceChunks,mutation);
const parentId='00000000-0000-4000-8000-000000000800',pin={objectId:parentId,revision:1};
const source=catalogFor('0.159.2'),time='2026-01-02T03:04:05.006Z';
const expected={operationId:'native-operation',callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:'0.159.2',method:'turn/interrupt',params:{threadId:'thread',turnId:'turn'}};
const observation={schemaVersion:1,...expected,nativeExecutableHash:source.catalog.nativeExecutableHash,
  requestHash:hashJson({method:expected.method,params:expected.params}),phase:'succeeded',createdAt:time,updatedAt:time,epoch:'epoch',requestId:42,reply:{result:{}},code:null};
const run={target:{serviceNodeId:expected.serviceNodeId},plan:{nativeVersion:expected.nativeVersion,catalogSourceHash:source.catalog.sourceHash}};
const request=await retainNativeRequest({rootObjectId:'root',client:{request(){throw Error('Unexpected inline RPC');}}},{nativeVersion:expected.nativeVersion,catalogSourceHash:source.catalog.sourceHash,method:expected.method,params:expected.params},expected.operationId);
const call={operationId:expected.operationId,request,expectedDefinitionHash:hashJson(source.definitions.get(expected.method))};
const base={parent:pin,expected,observation,run,call};
const rows=[];
function add(name,mode='save',patch={}){rows.push({name,mode,...patch});}
add('save pinned Run');add('save Run ID','save',{parent:parentId});add('read pinned Run','read');add('read Run ID','read',{parent:parentId});add('replay immutable save','replay');
for(const key of Object.keys(expected)) {
  add('expected '+key,'save',{expected:{[key]:key==='params'?{threadId:'other'}:'other'}});
  if(!['nativeVersion','serviceNodeId'].includes(key))add('read expected '+key,'read',{expected:{[key]:key==='params'?{threadId:'other'}:'other'}});
}
for(const key of ['operationId','callerPrincipalId','serviceNodeId','nativeVersion','nativeExecutableHash','method','requestHash','phase','epoch']) {
  add('body '+key,'save',{observation:{[key]:key.endsWith('Hash')?'sha256:'+'0'.repeat(64):'other'}});
  add('header '+key,'read',{manifest:{[key]:key.endsWith('Hash')?'sha256:'+'0'.repeat(64):'other'}});
}
add('invalid timestamp','save',{observation:{updatedAt:'2026-01-02'}});add('finish before create','save',{observation:{updatedAt:'2025-01-02T03:04:05.006Z'}});
add('error reply retained','save',{observation:{phase:'failed',code:'native_failed',reply:{error:{code:-32601,message:'unsupported'}}}});
add('error reply roundtrip','read',{initialObservation:{phase:'failed',code:'native_failed',reply:{error:{code:-32601,message:'unsupported'}}}});
add('missing required field','save',{remove:'requestId'});
add('extra field','save',{observation:{extra:true}});
add('wrong Run owner','save',{run:{target:{serviceNodeId:'other'}}});
add('wrong Run version','read',{run:{plan:{...run.plan,nativeVersion:'0.142.3'}}});
add('missing catalog source','save',{run:{plan:{...run.plan,catalogSourceHash:'sha256:'+'0'.repeat(64)}}});
add('empty catalog source is refused','save',{run:{plan:{...run.plan,catalogSourceHash:''}}});
add('large read','read',{large:true});add('partial chunks retained','save',{large:true,fail:'chunk-1'});
for(const fail of ['scope','manifest-write'])add('save failure '+fail,'save',{fail});
for(const fail of ['scope','manifest-read','blob-read'])add('read failure '+fail,'read',{fail});
add('scope before canonical','save',{wide:true,fail:'scope'});
for(const artifact of [{mediaType:'text/plain'},{contentHash:'sha256:'+'0'.repeat(64)},{extra:true},{object:null}])add('artifact '+JSON.stringify(artifact),'read',{artifact});
add('manifest wrong parent','read',{wrongParent:true});
add('missing chunk','read',{manifest:{chunks:[]}});
add('mismatched total hash','read',{manifest:{contentHash:'sha256:'+'0'.repeat(64)}});
add('mismatched total length','read',{manifest:{byteLength:1}});
add('wrong blob','read',{wrongBlob:true});
add('invalid artifact before scope','read',{artifact:{extra:true},fail:'scope'});
add('save exact NativeCall','save',{useCall:true});add('read exact NativeCall','read',{useCall:true});add('NativeCall read failure falls back to Run','save',{fail:'call-scope'});
add('prevented original operation roundtrip','read',{initialObservation:{phase:'failed',code:'request_prevented',epoch:null,requestId:null,reply:null}});
add('accepted original operation is retained','save',{observation:{phase:'accepted',epoch:null,requestId:null,reply:null}});
for(const key of ['operationId','expectedDefinitionHash'])add('saved call '+key,'save',{useCall:true,call:{[key]:key==='operationId'?'other':'sha256:'+'0'.repeat(64)}});
for(const key of ['catalogHash','nativeExecutableHash','method','params'])add('saved call reference '+key,'save',{useCall:true,call:{request:{...request,params:{...request.params,[key]:key==='method'?'thread/resume':key==='params'?{threadId:'other',turnId:'turn'}:'sha256:'+'0'.repeat(64)}}}});
add('saved call request source','save',{useCall:true,call:{request:{...request,catalogSourceHash:'sha256:'+'0'.repeat(64)}}});
add('saved call definition admission before canonical','save',{useCall:true,call:{expectedDefinitionHash:'sha256:'+'0'.repeat(64)},wide:true});
for(const row of rows) {
  let trace=[],document=null,active=false,blobs=[];
  const fail=(kind)=>{if(active&&row.fail===kind)throw new IvyError('fixture_failed',kind);};
  const store={
    rootObjectId:'root',client:{request(){throw Error('Unexpected inline RPC');}},
    async read(key,address,upgrade=true){trace.push(['read',key,address,upgrade]);if(key==='task-board/native-call'){fail('call-scope');if(row.useCall)return {value:active?{...call,...row.call}:call};throw new IvyError('not_found','No NativeCall fixture.');}if(key==='task-board/run'){fail('scope');return {value:active?{...run,...row.run}:run};}fail('manifest-read');return document;},
    async write(key,value,id,destination){trace.push(['write',key,value,id,destination]);fail('manifest-write');document={value,metadata:{parentId:destination.create.parentId},revision:{contentHash:hashJson(value)}};return {objectId:'00000000-0000-4000-8000-000000000900',revision:1};},
    saveLocalBlob(key,name,parent,bytes){trace.push(['blob-save',key,name,parent,digest(bytes),bytes.length]);fail('chunk-'+blobs.length);let entry=blobs.find(b=>b.name===name);if(!entry){entry={name,bytes:Buffer.from(bytes),pin:{objectId:'00000000-0000-4000-8000-'+String(blobs.length+1).padStart(12,'0'),revision:1}};blobs.push(entry);}return entry.pin;},
    localBlob(key,pin,parent){trace.push(['blob-read',key,pin,parent]);fail('blob-read');return row.wrongBlob?Buffer.from('wrong'):blobs.find(b=>b.pin.objectId===pin.objectId).bytes;},
  };
  const subject=new Evidence(store),parent=row.parent??pin;
  let obs={...observation,...row.initialObservation};if(row.large)obs.reply={result:{text:'x'.repeat(1100000)}};
  let result;
  try {
    if(row.mode==='read') {
      let artifact=await subject.save(parent,obs,expected);trace=[];active=true;
      document.value={...document.value,...row.manifest};document.revision.contentHash=hashJson(document.value);
      artifact={...artifact,contentHash:document.revision.contentHash,...row.artifact};
      if(row.wrongParent)document.metadata.parentId='other';
      result=await subject.read(parent,artifact,{...expected,...row.expected});
    } else {
      active=true;obs={...obs,...row.observation};if(row.remove)delete obs[row.remove];if(row.wide)obs.reply={result:Array(400001).fill(0)};
      result=await subject.save(parent,obs,{...expected,...row.expected});
      if(row.mode==='replay')result=await subject.save(parent,obs,expected);
    }
    result={ok:result};
  } catch(e){if(!(e instanceof IvyError))throw new Error('Original fixture failed in '+row.name,{cause:e});result={error:{code:e.code,message:e.message}};}
  if(['save pinned Run','read pinned Run','read Run ID','replay immutable save','save exact NativeCall','read exact NativeCall','prevented original operation roundtrip','accepted original operation is retained','error reply roundtrip'].includes(row.name)&&!result.ok)throw Error('Original positive evidence case failed: '+row.name+JSON.stringify(result));
  row.expectedHash=hashJson({result,trace,manifest:document?.value??null});
}
if(rows.length<70)throw Error('Insufficient operation-evidence cases');process.stdout.write(JSON.stringify({base,rows}));
