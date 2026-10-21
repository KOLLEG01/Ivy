// Inert NAB-3P protocol harness. Never constructs makeWASocket or opens a network.
import net from 'node:net';
import http from 'node:http';
import https from 'node:https';
import tls from 'node:tls';
import dgram from 'node:dgram';
import {syncBuiltinESMExports,createRequire} from 'node:module';
import {readFileSync,writeFileSync,existsSync} from 'node:fs';
for(const name of ['log','debug','trace','info','warn','error','dir','table','assert'])console[name]=()=>{};
let stopped=false,initialized=false,adapter,authority;
function fence(){if(!stopped){stopped=true;process.exit(2);}}
function forbidden(){fence();throw Error('Network is disabled in the synthetic protocol fixture.');}
net.connect=net.createConnection=net.Socket.prototype.connect=forbidden;
http.request=http.get=https.request=https.get=tls.connect=dgram.createSocket=forbidden;
globalThis.fetch=forbidden;syncBuiltinESMExports();
process.on('uncaughtException',fence);process.on('unhandledRejection',fence);
const {authFrames}=await import('../../../services/chat-bridge/rust/node/auth-frames.mjs');
const {createProtocolAuth,protocolAuthority,encode,decode,digest}=await import('../../../services/chat-bridge/rust/node/protocol-auth.mjs');
const local=createRequire(import.meta.resolve('baileys'));
if(JSON.parse(readFileSync(new URL('../package.json',import.meta.resolve('baileys')))).version!=='7.0.0-rc13'||JSON.parse(readFileSync(local.resolve('libsignal/package.json'))).version!=='6.0.0')fence();
const frames=authFrames(process.stdin,process.stdout,process.argv[2]);
let callback=0,closing=false;
const pending=new Map(),running=new Map(),gateUsed=new Set();
const gateRoot=process.env.IVY_CHAT_PROTOCOL_GATE;
const trace=(event,id,method)=>{if(gateRoot)writeFileSync(gateRoot+'.trace',JSON.stringify({event,id,method})+'\n',{flag:'a'});};
async function gate(name){
  if(!gateRoot)throw Error('Missing explicit fixture gate.');
  writeFileSync(gateRoot+'.'+name+'.observed','pending');
  while(!existsSync(gateRoot+'.'+name+'.release'))await new Promise(resolve=>setTimeout(resolve,10));
}
async function request(method,args){
  if(stopped)throw Error('Stopped protocol fixture.');
  const id='callback:'+String(++callback);
  const response=new Promise(resolve=>pending.set(id,{resolve,method,args}));
  await frames.send(id,{kind:'request',method,arguments:args});
  return response;
}
authority=protocolAuthority(request,fence);
async function action(method,args){
  if(method==='initialize'){
    if(initialized||adapter)throw Error('Repeated initialization.');
    adapter=await createProtocolAuth(args,authority,async()=>[]);initialized=true;
    return {baileys:'7.0.0-rc13',libsignal:'6.0.0',liveSocket:false,network:false};
  }
  if(!initialized)throw Error('Uninitialized protocol fixture.');
  switch(method){
    case 'invoke':{
      const target=args.target==='lid'?adapter.repository.lidMapping:args.target==='keys'?adapter.state.keys:adapter.repository;
      const allowed=args.target==='lid'?['storeLIDPNMappings','getLIDForPN','getLIDsForPNs','getPNForLID','getPNsForLIDs']:args.target==='keys'?['get','set']:['decryptGroupMessage','processSenderKeyDistributionMessage','decryptMessage','encryptMessage','encryptGroupMessage','getSenderKeyDistributionMessage','hasSenderKey','getSessionInfo','injectE2ESession','validateSession','deleteSession','migrateSession','jidToSignalProtocolAddress'];
      if(!allowed.includes(args.method))throw new TypeError('Unsupported synthetic operation.');
      return target[args.method](...args.arguments);
    }
    case 'credentials':Object.assign(adapter.state.creds,args);await adapter.saveCredentials();return {saved:true};
    case 'batch':return authority.raw('set',args);
    case 'nested':return adapter.state.keys.transaction(async()=>{
      await adapter.state.keys.set({'future-category':{'nested-buffered':{opaque:'\udfff',counter:3}}});
      const decrypted=await adapter.repository.decryptMessage(args.inbound);
      const encrypted=await adapter.repository.encryptMessage(args.outbound);
      await gate('nested-return');
      return {decrypted,encrypted,outerStillRequired:true};
    },'synthetic-outer-message-send');
    case 'event':return authority.span('inbound-crypto','synthetic.event',async()=>{
      await adapter.state.keys.set({'future-category':{'event':args}});
      return request('event',{...authority.current(),value:{event:'inert-protocol',value:args}});
    });
    case 'surface':return {methods:Object.keys(adapter.repository).filter(k=>typeof adapter.repository[k]==='function').sort(),sharedCreds:adapter.state.creds===adapter.state.creds,liveSocket:false};
    case 'corrupt':{
      const bytes=Buffer.from(JSON.stringify({v:1,g:'wrong-epoch',r:'callback:stale',m:'1',p:0,e:true,b:Buffer.from('{}').toString('base64')}));const header=Buffer.alloc(4);header.writeUInt32BE(bytes.length);process.stdout.write(Buffer.concat([header,bytes]));return new Promise(()=>{});
    }
    case 'child-exit':process.exit(2);break;
    case 'invalid-callback':{
      const grant=authority.current(),encoded='[]';
      return request('raw',{...grant,ordinal:args==='ordinal'?'01':'1',kind:'set',encoded,digest:args==='digest'?'sha256:'+'0'.repeat(64):'sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945',...(args==='token'?{token:'not-an-admitted-grant'}:{})});
    }
    case 'unexpected-network':forbidden();break;
    default:throw new TypeError('Unsupported synthetic protocol command.');
  }
}
try{
  for await(const{request:id,value}of frames.messages()){
    if(value.kind==='response'){
      const call=pending.get(id);if(!call)fence();pending.delete(id);
      const deliver=async()=>{
        if(initialized&&call.method==='raw'&&call.args.kind==='set'&&process.env.IVY_CHAT_PROTOCOL_GATE_MODE==='ack'&&!gateUsed.has('ack')){gateUsed.add('ack');await gate('ack');}
        call.resolve(value.result);
      };
      void deliver().catch(fence);continue;
    }
    if(value.kind==='closing'){if(closing)fence();closing=true;authority.closeAdmission();continue;}
    if(value.kind==='stop'){
      if(!closing||running.size||pending.size||authority.active())fence();
      if(process.env.IVY_CHAT_PROTOCOL_FAULT==='stop-ack')process.exit(2);
      adapter.repository.close();trace('stop',value.id,'quiesce');
      if(process.env.IVY_CHAT_PROTOCOL_GATE_MODE==='exit')await gate('exit');
      await new Promise((resolve,reject)=>process.stderr.write(Buffer.alloc(256*1024,0x2e),error=>error?reject(error):resolve()));
      await frames.send(id,{kind:'stopped',id:value.id});
      if(process.env.IVY_CHAT_PROTOCOL_FAULT==='late-stop'){
        const encoded=encode([['future-category','late-after-stop','1']]);
        await frames.send('callback:'+String(++callback),{kind:'request',method:'raw',arguments:{id:value.id,token:value.token,ordinal:'1',kind:'set',encoded,digest:digest(encoded)}});
      }
      stopped=true;process.stdin.destroy();break;
    }
    if(value.kind!=='command'||closing||running.has(id)||running.size>=4||id!==value.id||typeof value.token!=='string')fence();
    const grant={id:value.id,token:value.token};trace('command',value.id,value.method);
    const run=(async()=>{
      let result,error;
      try{result=await authority.run(grant,()=>action(value.method,decode(JSON.stringify(value.arguments))));}
      catch(e){error=e;}
      await authority.finishRoot(grant,result,error);
      await frames.send(id,error?{kind:'error',id:value.id,name:authority.safeName(error)}:{kind:'result',id:value.id,encoded:encode(result)});
    })();
    running.set(id,run);run.then(()=>running.delete(id),fence);
  }
  if(!stopped)fence();
}catch{fence();}
