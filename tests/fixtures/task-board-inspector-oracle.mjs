import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {hashJson,canonical}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {serviceTools}=await import('../../packages/sdk/src/client.ts');
const {validateAgent}=await import('../../packages/contracts/src/agent-validation.ts');
const {validateNativeRead,validateNativeStatus}=await import('../../packages/sdk/src/native-evidence.ts');
const {checkedNativeContract:catalogFor}=await import('../../packages/contracts/src/checked-native-contract.ts');
const load=n=>readFileSync('services/task-board/src/runtime/'+n+'.ts','utf8').replace(/^import .*;\r?\n/gm,'').replaceAll('export ','');
const Evidence=new Function('catalogFor','validateNativeRead','requireThat',stripTypeScriptTypes(load('read-evidence'),{mode:'transform'})+'\nreturn TaskBoardReadEvidence;')(catalogFor,validateNativeRead,requireThat);
const turnSource=load('turn-evidence');const helpers=turnSource.slice(turnSource.indexOf('const pageCursor'),turnSource.indexOf('/** Verifies read artifacts'));
const {pageCursor,turnsParams}=new Function('requireThat',stripTypeScriptTypes(helpers)+'\nreturn {pageCursor,turnsParams};')(requireThat);
const mutationSource=readFileSync('services/task-board/src/runtime/store.ts','utf8').match(/export const mutation = [\s\S]*?;/)[0].replace('export ','');
const mutation=new Function('hashJson',stripTypeScriptTypes(mutationSource)+'\nreturn mutation;')(hashJson);
const same=(a,b)=>a===undefined||b===undefined?a===b:canonical(a)===canonical(b);
const nativeArray=v=>Array.isArray(v)?v:[],nativeRecord=v=>v!==null&&typeof v==='object'&&!Array.isArray(v)?v:{};
const time='2026-01-02T03:04:05.006Z';class ClockDate extends Date{constructor(){super(time);}}
const Inspector=new Function('hashJson','requireThat','validateAgent','serviceTools','validateNativeStatus','same','catalogFor','nativeArray','nativeRecord','TaskBoardReadEvidence','mutation','pageCursor','turnsParams','Date',stripTypeScriptTypes(load('native-inspector'),{mode:'transform'})+'\nreturn TaskBoardNativeInspector;')
  (hashJson,requireThat,validateAgent,serviceTools,validateNativeStatus,same,catalogFor,nativeArray,nativeRecord,Evidence,mutation,pageCursor,turnsParams,ClockDate);
const source=catalogFor('0.159.2'),target={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:'0.159.2',nativeExecutableHash:source.catalog.nativeExecutableHash,catalogHash:source.catalogHash};
const pin={objectId:'00000000-0000-4000-8000-000000000800',revision:2};
const run={target:{serviceNodeId:target.serviceNodeId,hostId:target.hostId},plan:{nativeVersion:target.nativeVersion,catalogSourceHash:source.catalog.sourceHash},primaryResourceRef:{namespace:'codex',kind:'thread',serviceNodeId:target.serviceNodeId,nativeId:'thread'},turnId:'turn',nativeEpoch:'epoch',originalRequest:{original:true}};
const status={...target,epoch:'epoch',pid:42,state:'ready',observedAt:time,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const context={run:pin,nativeVersion:target.nativeVersion,catalogSourceHash:source.catalog.sourceHash,serviceNodeId:target.serviceNodeId,epoch:'epoch',threadId:'thread',turnId:'turn'};
const page=(data=[],nextCursor=null)=>({reply:{result:{data,nextCursor}}});
const found=()=>page([{id:'turn',status:'completed'}]);
const metadata=()=>({reply:{result:{thread:{id:'thread',ephemeral:false}}}});
const saved=()=>({pin:{objectId:'00000000-0000-4000-8000-000000000900',revision:1},value:{schemaVersion:1,...context,run:{...pin,revision:1},phase:'searching',nextCursor:'next',pageCount:1,visitedCursorHashes:[hashJson(null)],createdAt:time,updatedAt:time}});
const rows=[];function add(name,input={}){rows.push({name,input:{run,context,status,steps:[found(),metadata(),found()],...input}});}
add('find fresh');add('find one',{pageSize:1});add('find absent',{steps:[page()]});
add('search checkpoint',{budget:1,steps:[page([],'next')]});
add('resume after checkpoint',{budget:1,passes:2,steps:[page([],'next'),found(),metadata(),found()]});
add('resume saved',{saved:saved()});add('finish saved absent',{saved:saved(),steps:[page()]});
add('restart finished',{saved:{...saved(),value:{...saved().value,phase:'finished',nextCursor:null}}});
for(const budget of [0,65,1.5,-1])add('budget '+budget,{budget});
for(const key of ['turnId','nativeEpoch','primaryResourceRef'])add('missing '+key,{run:{...run,[key]:null}});
add('wrong primary owner',{run:{...run,primaryResourceRef:{...run.primaryResourceRef,serviceNodeId:'other'}}});
add('invalid native owner',{status:{...status,serviceNodeId:'other'}});
add('catalog mismatch',{status:{...status,catalogHash:'sha256:'+'0'.repeat(64)}});
add('owner verification fails',{failOwner:1});add('checkpoint owner lost',{budget:1,steps:[page([],'next')],failOwner:3});
add('loop cursor',{steps:[page([],'next'),page([],'next')]});
add('bad cursor',{steps:[page([],17)]});
add('duplicate turn',{steps:[page([{id:'turn'},{id:'turn'}])]});
add('page too large',{pageSize:1,steps:[page([{id:'a'},{id:'b'}])]});
add('fresh page too large',{pageSize:1,steps:[found(),metadata(),page([{id:'a'},{id:'turn'}])]});
add('moved turn',{steps:[found(),metadata(),page([{id:'other'}])]});
add('ephemeral thread',{steps:[found(),{reply:{result:{thread:{id:'thread',ephemeral:true}}}},found()]});
add('wrong thread',{steps:[found(),{reply:{result:{thread:{id:'other',ephemeral:false}}}},found()]});
add('backwards final observation',{steps:[found(),metadata(),{...found(),observedAt:'2026-01-02T03:04:05.005Z'}]});
add('wrong read epoch',{steps:[{...found(),epoch:'other'}]});
add('wrong read request hash',{steps:[{...found(),requestHash:'sha256:'+'0'.repeat(64)}]});
add('native error reply',{steps:[{reply:{error:{code:-32601,message:'unsupported'}}}]});
for(const [key,value] of [['threadId','other'],['epoch','other'],['pageCount',2],['run',{...pin,revision:3}]])add('saved mismatch '+key,{saved:{...saved(),value:{...saved().value,[key]:value}}});
add('original request mismatch',{saved:saved(),originalRun:{...run,originalRequest:{original:false}}});
add('saved cursor loop',{saved:{...saved(),value:{...saved().value,nextCursor:null}}});
add('search bound',{saved:{...saved(),value:{...saved().value,pageCount:1024,visitedCursorHashes:Array.from({length:1024},(_,i)=>hashJson(i))}}});
add('write failure',{budget:1,steps:[page([],'next')],writeFailure:true});
add('read allow error',{action:'read',allowError:true,steps:[{reply:{error:{code:-32601,message:'unsupported'}}}]});
add('owner stopped no epoch',{action:'stopped',run:{...run,nativeEpoch:null}});add('owner same epoch',{action:'stopped'});
const connection={mode:'owned-stdio',ownsServer:true,ownsHome:true,actualHome:'fixture',actualVersion:target.nativeVersion,socketPath:null,serverPid:42,lifecycle:'owned-stdio',mcpIdentity:'instance-environment'};
add('owned predecessor stopped',{action:'stopped',status:{...status,epoch:'replacement',connection,processStops:[{epoch:'epoch',observedAt:time}]}});
add('external proxy not stop proof',{action:'stopped',status:{...status,epoch:'replacement',connection:{...connection,mode:'external-proxy',ownsServer:false},processStops:[{epoch:'epoch',observedAt:time}]}});
for(const row of rows){const i=row.input,trace=[];let checkpoint=structuredClone(i.saved??null),reads=0,owners=0;const writes=[];
 const client={async request(method,params){trace.push(['rpc',method,structuredClone(params)]);if(method==='tools.list')return {provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:params.namespace+'.'+params.namePrefix,definition:{namespace:params.namespace,name:params.namePrefix,interfaceVersion:'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null};
   if(params.qualifiedName==='agent.status')return i.status;
   const args=params.arguments,step=i.steps[reads++];if(!step)throw Error('Missing fixture read');
   return {schemaVersion:1,observationId:'read-'+reads,callerPrincipalId:'caller',serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,catalogHash:target.catalogHash,epoch:'epoch',requestId:reads,observedAt:time,method:args.method,params:args.params,requestHash:hashJson({method:args.method,params:args.params}),...step};
 }};
 const store={client,async read(key,address,upgrade=true){trace.push(['read',key,address,upgrade]);return {pin:address,value:address.revision===1?(i.originalRun??i.run):i.run};},
   async named(key,name,parent){trace.push(['named',key,name,parent]);return checkpoint;},
   async write(key,value,id,destination){trace.push(['write',key,value,id,destination]);if(i.writeFailure)throw new IvyError('fixture_failed','write');const result={objectId:'00000000-0000-4000-8000-000000000900',revision:(checkpoint?.pin.revision??0)+1};checkpoint={pin:result,value};writes.push(checkpoint);return result;}};
 const engine={store,settings:{principalId:'caller'},async verifyOwner(){trace.push(['owner']);if(++owners===i.failOwner)throw new IvyError('fixture_failed','owner');}};
 const subject=new Inspector(engine);let result;
 try{const values=[];for(let pass=0;pass<(i.passes??1);pass++)values.push(i.action==='stopped'?await subject.ownerStopped(i.run):i.action==='read'?await subject.read(i.context,'thread/turns/list',turnsParams('thread',null),i.allowError):await subject.find(pin,i.budget??8,i.pageSize??100));result={ok:values};}
 catch(e){result={error:{code:e.code,message:e.message}};}
 row.expectedHash=hashJson({result,trace,writes});
}
process.stdout.write(JSON.stringify({pin,time,rows}));
