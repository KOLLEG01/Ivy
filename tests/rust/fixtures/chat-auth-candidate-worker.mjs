// Explicit inert candidate. This file never constructs a socket or fetches a version.
import net from 'node:net';
import http from 'node:http';
import https from 'node:https';
import tls from 'node:tls';
import dgram from 'node:dgram';
import {syncBuiltinESMExports,createRequire} from 'node:module';
import {readFileSync,writeFileSync,existsSync} from 'node:fs';
for(const n of ['log','debug','trace','info','warn','error','dir','table','assert'])console[n]=()=>{};
let stopped=false,initialized=false,adapter,authority,wrappedKeys,runtimeGrant,factoryGraph;
function fence(){if(!stopped){stopped=true;process.exit(2);}}
function forbidden(){fence();throw Error('No network in the inert candidate.');}
net.connect=net.createConnection=net.Socket.prototype.connect=forbidden;
http.request=http.get=https.request=https.get=tls.connect=dgram.createSocket=forbidden;
globalThis.fetch=forbidden;syncBuiltinESMExports();
process.on('uncaughtException',fence);process.on('unhandledRejection',fence);
const {authFrames}=await import('../../../services/chat-bridge/rust/node/auth-frames.mjs');
const {candidateAuthority,prepareRawAuth,candidateSocketOptions,encode,decode,digest}=await import('../../../services/chat-bridge/rust/node/protocol-candidate-auth.mjs');
const {addTransactionCapability,DEFAULT_CONNECTION_CONFIG}=await import('baileys');
const local=createRequire(import.meta.resolve('baileys'));
if(JSON.parse(readFileSync(new URL('../package.json',import.meta.resolve('baileys')))).version!=='7.0.0-rc13'||JSON.parse(readFileSync(local.resolve('libsignal/package.json'))).version!=='6.0.0')fence();
const frames=authFrames(process.stdin,process.stdout,process.argv[2]);
const pending=new Map(),running=new Map(),closingWork=new Set(),gateUsed=new Set();
let callback=0,closing=false,repository,rawBeforeWrapper=false,lateRefused=false;
const gateRoot=process.env.IVY_CHAT_CANDIDATE_GATE;
const trace=(event,id,method)=>{if(gateRoot)writeFileSync(gateRoot+'.trace',JSON.stringify({event,id,method})+'\n',{flag:'a'});};
async function gate(name){if(!gateRoot)throw Error('Missing candidate gate.');writeFileSync(gateRoot+'.'+name+'.observed','pending');while(!existsSync(gateRoot+'.'+name+'.release'))await new Promise(r=>setTimeout(r,10));}
async function request(method,args){
  if(stopped)throw Error('Stopped candidate.');
  const id='callback:'+String(++callback),response=new Promise(resolve=>pending.set(id,{resolve,method,args}));
  await frames.send(id,{kind:'request',method,arguments:args});return response;
}
authority=candidateAuthority(request,fence);
const quiet={trace(){},debug(){},info(){},warn(){},error(){},child(){return this;}};
async function action(method,args,runtime){
  if(method==='initialize'){
    if(initialized)throw Error('Repeated candidate initialization.');authority.setRuntime(runtime);runtimeGrant=runtime;
    adapter=await prepareRawAuth(args,authority);
    rawBeforeWrapper=adapter.state.keys===adapter.raw&&adapter.state.keys.transaction===undefined;
    wrappedKeys=addTransactionCapability(adapter.state.keys,quiet,DEFAULT_CONNECTION_CONFIG.transactionOpts);
    factoryGraph={creds:adapter.state.creds,keys:wrappedKeys};repository=adapter.makeSignalRepository(factoryGraph,quiet,async()=>[]);
    if(!rawBeforeWrapper)throw Error('Candidate supplied a pre-wrapped graph.');initialized=true;
    if(process.env.IVY_CHAT_CANDIDATE_GATE_MODE==='ready')await gate('ready');
    return {baileys:'7.0.0-rc13',libsignal:'6.0.0',candidate:true,rawBeforeWrapper,liveSocket:false,network:false};
  }
  if(!initialized)throw Error('Uninitialized candidate.');
  switch(method){
    case 'surface':{
      let duplicateRefused=false;try{adapter.makeSignalRepository({creds:adapter.state.creds,keys:wrappedKeys},quiet,async()=>[]);}catch{duplicateRefused=true;}
      return {methods:Object.keys(repository).filter(k=>typeof repository[k]==='function').sort(),rawBeforeWrapper,duplicateRefused,oneCredsObject:factoryGraph.creds===adapter.state.creds&&factoryGraph.keys===wrappedKeys&&adapter.state.keys===adapter.raw,liveSocket:false};
    }
    case 'invoke':{
      const target=args.target==='lid'?repository.lidMapping:args.target==='keys'?wrappedKeys:repository;
      const allowed=args.target==='lid'?['storeLIDPNMappings','getLIDForPN','getLIDsForPNs','getPNForLID','getPNsForLIDs']:args.target==='keys'?['get','set']:['decryptGroupMessage','processSenderKeyDistributionMessage','decryptMessage','encryptMessage','encryptGroupMessage','getSenderKeyDistributionMessage','hasSenderKey','getSessionInfo','injectE2ESession','validateSession','deleteSession','migrateSession','jidToSignalProtocolAddress'];
      if(!allowed.includes(args.method))throw new TypeError('Unsupported candidate operation.');return target[args.method](...args.arguments);
    }
    case 'batch':return authority.raw('set',args);
    case 'credentials':Object.assign(adapter.state.creds,args);await adapter.saveCredentials();return {saved:true};
    case 'nested':return wrappedKeys.transaction(async()=>{
      await wrappedKeys.set({'future-category':{'candidate-buffered':{opaque:'\udfff',counter:3}}});
      const decrypted=await repository.decryptMessage(args.inbound),encrypted=await repository.encryptMessage(args.outbound);
      await gate('nested-return');return {decrypted,encrypted};
    },'candidate-outer-send');
    case 'event':return authority.span('inbound-crypto','candidate.event',()=>request('event',{...authority.current(),value:args}));
    case 'missing-context':{
      await gate('missing-context');return authority.run({id:'not-admitted',token:'not-admitted'},()=>authority.raw('set',[['future-category','missing-context','1']]));
    }
    case 'gate':await gate('work');return {done:true};
    case 'get-message':return (await candidateSocketOptions(adapter,[2,3000,1],quiet,authority).getMessage(args))??null;
    // Inert wire endpoints exercise Rust policy only; no live socket is claimed.
    case 'open':return {user:args.user,online:true,privacy:{online:'all',lastSeen:'all'},synthetic:true};
    case 'sendMessage':trace('send',args.messageId,'sendMessage');return {id:args.messageId,synthetic:true};
    case 'readMessages':trace('read',String(args.length),'readMessages');return null;
    case 'presence':trace('presence',args.jid??'',args.state);return null;
    case 'privacy':return {online:'all',lastSeen:'all'};
    default:throw new TypeError('Unsupported candidate command.');
  }
}
try{
  for await(const{request:id,value}of frames.messages()){
    if(value.kind==='response'){
      const call=pending.get(id);if(!call)fence();pending.delete(id);
      const deliver=async()=>{
        if(initialized&&call.method==='raw'&&call.args.kind==='set'&&process.env.IVY_CHAT_CANDIDATE_GATE_MODE==='ack'&&!gateUsed.has('ack')){gateUsed.add('ack');await gate('ack');}
        call.resolve(value.result);
      };void deliver().catch(fence);continue;
    }
    if(value.kind==='closing'){
      if(closing)fence();closing=true;authority.closeAdmission(value.close,value.cleanup);
      const cleanup=(async()=>{
        // A fresh task citing the old runtime/ALS is deliberately refused.
        try{await authority.span('background-prekey','late.timer',()=>{throw Error('Fresh work executed.');});}catch(e){lateRefused=e.code==='auth_retiring';}
        const denied=await request('begin',{kind:'background-prekey',method:'late.bare-runtime',parent:runtimeGrant});
        const eventDenied=await request('event',{...runtimeGrant,value:{type:'late.unsolicited'}});
        lateRefused=lateRefused&&denied?.refused===true&&eventDenied?.refused===true;
        trace('late-refused',String(lateRefused),'late.timer');
        if(process.env.IVY_CHAT_CANDIDATE_GATE_MODE==='cleanup')await gate('cleanup');
        await authority.tokenIndexCleanup(()=>authority.raw('set',[['tctoken','index','{"candidateClose":true}']]));
        trace('cleanup',value.close.id,'token-index.flush');
      })();closingWork.add(cleanup);cleanup.then(()=>closingWork.delete(cleanup),fence);continue;
    }
    if(value.kind==='stop'){
      await Promise.all([...running.values(),...closingWork]);
      const tracking=authority.tracking();
      if(!closing||pending.size||tracking.active||tracking.reservations||!tracking.cleanupUsed||!lateRefused)fence();
      if(process.env.IVY_CHAT_CANDIDATE_FAULT==='stop-ack')process.exit(2);
      repository.close();trace('stop',value.id,'quiesce');
      if(process.env.IVY_CHAT_CANDIDATE_GATE_MODE==='exit')await gate('exit');
      await new Promise((resolve,reject)=>process.stderr.write(Buffer.alloc(256*1024,0x2e),e=>e?reject(e):resolve()));
      await frames.send(id,{kind:'stopped',id:value.id,scope:'candidate.inert.v1'});
      if(process.env.IVY_CHAT_CANDIDATE_FAULT==='late-stop'){
        const encoded=encode([['future-category','late-after-stop','1']]);await frames.send('callback:'+String(++callback),{kind:'request',method:'raw',arguments:{id:value.id,token:value.token,ordinal:'1',kind:'set',encoded,digest:digest(encoded)}});
      }
      stopped=true;process.stdin.destroy();break;
    }
    if(value.kind!=='command'||closing||running.has(id)||id!==value.id)fence();
    const grant={id:value.id,token:value.token};trace('command',id,value.method);
    const work=(async()=>{
      let result,error;try{result=await authority.run(grant,()=>action(value.method,decode(JSON.stringify(value.arguments)),value.runtime));}catch(e){if(e.code==='auth_retiring')fence();error=e;}
      await authority.finishRoot(grant,result,error);
      await frames.send(id,error?{kind:'error',id,name:authority.safeName(error)}:{kind:'result',id,encoded:encode(result)});
    })();running.set(id,work);work.then(()=>running.delete(id),fence);
  }
  if(!stopped)fence();
}catch{fence();}
