// The real autonomous service resumes retained original Main/input checkpoints.
import{starting,now,instructions,noticeInstructions,key,startChatBridge,IvyError,toolDefinitionHash,canonical}from'./chat-service-runtime-loader.mjs';
import{environment,clone}from'./chat-main-oracle.mjs';
import{extension as deliveryExtension}from'./chat-delivery-oracle.mjs';
import{readFileSync,existsSync}from'node:fs';
import{once}from'node:events';
import{createInterface}from'node:readline';
import{setTimeout as delay}from'node:timers/promises';
import{WebSocketServer}from'ws';
import{DatabaseSync}from'node:sqlite';
const error=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome,...(e.details===undefined?{}:{details:e.details})};};
const until=async(f,label)=>{for(let n=0;n<2000;n++){const v=await f();if(v)return v;await delay(5);}throw Error('Timed out '+label);};
let scope,serial=0;const sockets=new Map(),pending=new Map();
const server=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});await once(server,'listening');
const base=`http://127.0.0.1:${server.address().port}/ivy/`;
const send=(s,v)=>{if(s.readyState===s.OPEN)s.send(JSON.stringify(v));};
function activeExtension(){
 const ext=deliveryExtension(),guard=ext.beforeRequest;
 ext.beforeRequest=async(method,p,event,c)=>{
  // Main replacement uses thread/resume too. Route its current durable Main
  // claim to environment's existing Main guard rather than the input guard.
  if(method==='tools.call'&&p.qualifiedName==='agent.invoke'&&p.arguments.method==='thread/resume'&&(await c.main.find())?.value.pendingAction)return false;
  return guard(method,p,event,c);
 };
 return ext;
}
function open(spec){
 if(scope)throw Error('Previous isolated case was not closed');
 const ext=activeExtension(),originalRpc=ext.rpc;let context;
 ext.opened=(opened=>c=>{context=c;opened(c);})(ext.opened);
 ext.rpc=async(method,p,c,next)=>{
  if(method==='settings.get')return{settings:{quietHours:null}};
  const v=await originalRpc(method,p,c,next);
  if(spec.kind==='cancel-running'&&method==='tools.call'&&p.qualifiedName==='agent.invoke'&&p.arguments.method==='turn/interrupt')c.controls.turnPhase='interrupted';
  return v;
 };
 const env=environment(spec.seed,false,ext);
 // Both service and synthetic peer inspect this same isolated SQLite file.
 // Existing fixture guards therefore verify the actual durable native claim.
 scope={spec,env,observer:new DatabaseSync(env.file,{readOnly:true}),get context(){return context;},calls:[],handshakes:[],heartbeats:[],notifications:[],bad:[],pending:0,generation:6,reads:0,hold:null,held:false,armReads:0};
 return{url:base,root:env.root};
}
function summary(){return{calls:scope.calls,handshakes:scope.handshakes,heartbeats:scope.heartbeats,notifications:scope.notifications,bad:scope.bad,reads:scope.reads,held:scope.held,readsAfterArm:scope.reads-scope.armReads,pending:scope.pending};}
async function quiet(){let previous='',stable=0;for(let n=0;n<1000;n++){const count=scope.calls.length+':'+scope.observer.prepare('PRAGMA data_version').get().data_version;if(!scope.pending&&count===previous){if(++stable>=20)return;}else stable=0;previous=count;await delay(10);}throw Error('Autonomous service did not settle');}
async function invoke(name,args){
 const[s,session]=[...sockets].find(([,v])=>v.generation===7)??[];if(!s||!session.registry)throw Error('No registered provider');
 const definition=session.registry.namespaces[0].tools.find(t=>t.name===name),id='active-'+(++serial),response=new Promise(done=>pending.set(id,done));
 send(s,{jsonrpc:'2.0',id,method:'provider.invoke',params:{qualifiedName:'chat.'+name,arguments:args,definitionHash:toolDefinitionHash(definition),generation:7,callerPrincipalId:'alice',...(!definition.annotations?.readOnlyHint?{operationId:args.operationId}:{})}});
 const frame=await response;delete frame.id;return frame;
}
async function command(c){
 if(c.action==='begin')return open(c.spec);
 if(c.action==='summary')return summary();
 if(c.action==='failure')return{report:summary(),snapshot:await scope.env.snapshot(null),files:['chat-bridge.sqlite','chat-bridge.sqlite-wal','chat-bridge.sqlite-shm'].filter(n=>existsSync(scope.env.root+'/'+n)).map(name=>({name,value:readFileSync(scope.env.root+'/'+name).toString('base64')})),root:scope.env.root};
 if(c.action==='state')return{main:(await scope.env.main.find())?.value??null,inputs:scope.context.records.filter(r=>r.object.contractKey==='chat-bridge/input').map(r=>r.content.value),collections:scope.observer.prepare("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/collection'").all().map(r=>JSON.parse(r.value_json)),reads:scope.reads};
 if(c.action==='quiet'){await quiet();return summary();}
 if(c.action==='invoke')return invoke(c.name,c.arguments);
 if(c.action==='arm'){
  scope.armReads=scope.reads;scope.held=false;let release;scope.hold={promise:new Promise(done=>release=done),release:()=>release()};return null;
 }
 if(c.action==='notify'){
  const frame={jsonrpc:'2.0',method:'notifications.provider',params:{namespace:'agent',name:'notification',version:'1.0.0',serviceNodeId:'owner',generation:4,payload:{method:c.method??'turn/completed',params:{turn:{id:c.turnId}}}}};
  scope.notifications.push(frame);for(const s of sockets.keys())send(s,frame);return null;
 }
 if(c.action==='release'){const hold=scope.hold;scope.hold=null;hold.release();return null;}
 if(c.action==='end'){
  await quiet();const snapshot=await scope.env.snapshot(c.output),report=summary();
  // Close the final fixture-owned handles before exporting the complete file;
  // copying only the main DB while its WAL is live omits committed changes.
  scope.observer.close();const database=scope.env.exportSeed().database;
  scope.env.close();scope=null;return{snapshot,database,report};
 }
 throw Error('Unexpected active fixture command '+c.action);
}
server.on('connection',socket=>{
 const owned=scope,session={generation:++owned.generation,registry:null};sockets.set(socket,session);socket.on('close',()=>sockets.delete(socket));
 socket.on('message',async bytes=>{
  const frame=JSON.parse(bytes.toString());if(!frame.method){pending.get(frame.id)?.(frame);pending.delete(frame.id);return;}
  const{id,method,params}=frame,call={method,params};owned.calls.push(call);owned.pending++;
  try{
   let value;
   if(method==='service.connect'){owned.handshakes.push({method,params});value={generation:session.generation};}
   else if(method==='registry.sync'){session.registry=params;owned.handshakes.push({method,params});value={};}
   else if(method==='notifications.subscribe'){owned.handshakes.push({method,params});value={};}
   else if(method==='service.heartbeat'){owned.heartbeats.push(params);value={ready:params.ready};}
   else{
    if(method==='tools.call'&&params.qualifiedName==='agent.read'&&params.arguments.method==='thread/turns/list'){
     owned.reads++;if(owned.hold&&!owned.held){owned.held=true;await owned.hold.promise;}
    }
    value=await owned.env.store.client.request(method,params);
   }
   call.value=value;send(socket,{jsonrpc:'2.0',id,result:value});
  }catch(e){if(!(e instanceof IvyError))owned.bad.push({method,params,error:String(e)});call.error=error(e);send(socket,{jsonrpc:'2.0',id,error:IvyError.from(e).toWire()});}
  finally{owned.pending--;}
 });
});
if(process.argv[2]==='peer'){
 console.log(JSON.stringify({url:base}));for await(const line of createInterface({input:process.stdin,crlfDelay:Infinity})){
  try{console.log(JSON.stringify({value:await command(JSON.parse(line))}));}catch(e){console.log(JSON.stringify({error:error(e)}));}
 }
}else{
 const cases=[],seeds={};
 for(const version of Object.keys(starting)){
  const env=environment(starting[version],false,activeExtension());try{
   const created=await env.run({action:'create'}),sent=await env.run({action:'send'});if(created.phase!=='succeeded'||sent.phase!=='succeeded')throw Error('Original seed admission failed');
   await env.run({action:'worker'});seeds['prepared-'+version]=env.exportSeed();
  }finally{env.close();}
  const running=environment(seeds['prepared-'+version],false,activeExtension());try{
   for(let n=0;n<8;n++){await running.run({action:'worker'});const main=await running.main.find(),input=await running.store.read('chat-bridge/input',main.value.queue[0].inputId);if(input.value.turnId&&await running.store.named('chat-bridge/collection','Chat original turn collection',input.pin.objectId))break;}
   const main=await running.main.find(),input=await running.store.read('chat-bridge/input',main.value.queue[0].inputId);if(!input.value.turnId)throw Error('Original running seed failed');
   seeds['running-'+version]=running.exportSeed();
  }finally{running.close();}
  cases.push({label:'terminal-'+version,kind:'terminal',version,seed:{...clone(seeds['running-'+version]),controls:{...seeds['running-'+version].controls,turnPhase:'completed'}}});
 }
 const version='0.154.0',add=(kind,seed=seeds['running-'+version])=>cases.push({label:kind,kind,version,seed:clone(seed)});
 add('deliver-prepared',seeds['prepared-'+version]);
 for(const kind of ['cancel-queued','cancel-running']){
  const initial=kind==='cancel-queued'?seeds['prepared-'+version]:seeds['running-'+version],env=environment(initial,false,activeExtension());try{await env.run({action:'cancel-first'});add(kind,env.exportSeed());}finally{env.close();}
 }
 for(const suffix of ['matching','foreign','other-method','none'])add('wake-'+suffix);
 for(const resumed of [false,true]){
  const env=environment(starting[version],false,activeExtension());try{
   await env.run({action:'create'});if(resumed){await env.run({action:'reconfigure'});await env.run({action:'resume',label:'resumed-create'});}
   const operation=await env.ops.find('alice',`chat:${now}:${resumed?'resumed-create':'create'}`),request=clone(operation.value.request);
   cases.push({label:resumed?'resumed-create-posthooks':'explicit-create-posthooks',kind:'create',version,request,seed:env.exportSeed()});
  }finally{env.close();}
 }
 for(const spec of cases){
  open(spec);const health=[],events=[];
  globalThis[key]={config:{componentId:'chat-bridge',credential:'fixture-token',dataRoot:scope.env.root,serviceNodeId:'chat-provider',hostId:'fixture-host',version:'1.0.0',buildId:'sha256:'+'a'.repeat(64),publicBaseUrl:base,settings:{definition:spec.seed.definition,pollMs:60000,whatsappConfigPath:'synthetic-account-lifecycle'}},
   async write(ready,details,generation){health.push({ready,details,generation:generation??null});},async control(){return null;},
   transport:{transport:{state:'fixture-connected'},journal:{set(key,value){events.push({created:key,value});}},start(){events.push('start');},attach(v){events.push({attach:v?.main.native.owner.generation??null});},changed(){events.push('changed');},notification(){events.push('notification');},async settled(){events.push('settled');},async close(){events.push('close');}}};
  const app=await startChatBridge('isolated-active-fixture'),outputs=[];await until(()=>app.service.ready,'ready');
  if(spec.kind==='deliver-prepared'||spec.kind.startsWith('wake-'))await until(()=>scope.reads>0,'original delivered input entered collection');
  await quiet();
  if(spec.kind.startsWith('wake-')){
   const op=await scope.env.ops.find('alice',`chat:${now}:main-send`);spec.sendRequest=clone(op.value.request);
   await command({action:'arm'});outputs.push(await invoke('send',spec.sendRequest));await until(()=>scope.held,'active native read');
   if(app.runtime.worker.turnId!=='delivery-turn-1')throw Error('Original turn is not visible during drain');
   if(spec.kind!=='wake-none')await command({action:'notify',method:spec.kind==='wake-other-method'?'turn/started':'turn/completed',turnId:spec.kind==='wake-foreign'?'foreign-turn':'delivery-turn-1'});
   outputs.push(await invoke('operation',{operationId:spec.sendRequest.operationId,expectedBridge:spec.sendRequest.expectedBridge}));
   await command({action:'release'});await quiet();spec.wakeReads=scope.reads-scope.armReads;
   if(spec.wakeReads!==(spec.kind==='wake-matching'?2:1))throw Error('Unexpected original wake count '+spec.kind+' '+spec.wakeReads);
  }else if(spec.kind==='create'){
   outputs.push(await invoke('createMain',spec.request));outputs.push(await invoke('createMain',spec.request));outputs.push(await invoke('createMain',{...spec.request,reason:'A different request cannot reuse this operation.'}));await quiet();
   const markers=events.filter(v=>v?.created);if(markers.length!==(spec.request.resumeMain?0:2))throw Error('Original create marker differs');
  }
  if(spec.kind==='terminal'||spec.kind.startsWith('cancel-'))await until(async()=>!(await app.runtime.main.find()).value.queue.length,'queue release');
  if(spec.kind==='deliver-prepared'){const main=await app.runtime.main.find(),input=await app.runtime.main.store.read('chat-bridge/input',main.value.queue[0].inputId);if(!input.value.turnId)throw Error('Original autonomous native delivery did not start');}
  await app.close();spec.health=health;spec.events=events;spec.output=outputs;spec.final=await command({action:'end',output:outputs});
  if(spec.final.report.bad.length||spec.final.report.heartbeats.some(h=>!h.ready||h.diagnostics.length))throw Error('Original worker/fixture failure '+spec.label+' '+canonical(spec.final.report.bad));
 }
 for(const s of server.clients)s.terminate();await new Promise(done=>server.close(done));
 process.stdout.write(JSON.stringify({now,instructions,noticeInstructions,cases,boundary:'Unchanged original main.ts with real ServiceClient/socket/worker/domain/native-evidence owners. Retained original checkpoints make seed identities exact. Only Host configuration/health and account lifecycle are probes; native/public RPCs use the existing isolated fixture, whose guards read the actual shared SQLite file before dispatch. Named local UUIDs/mutation labels are normalized by the existing snapshot; no arbitrary hash normalization. No account/installed/platform acceptance.'}));
}
