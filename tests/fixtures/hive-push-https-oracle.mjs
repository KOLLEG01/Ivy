// Isolated HTTPS peer and the pinned original web-push sender. No live provider.
import https from 'node:https';
import {createRequire} from 'node:module';
import {readFileSync} from 'node:fs';
import {createInterface} from 'node:readline';
import crypto from 'node:crypto';
import net from 'node:net';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';

const profile=JSON.parse(process.env.IVY_HIVE_PUSH_PROFILE??'null');
const profiles=profile?JSON.parse(readFileSync(new URL('./hive-push-certificate-profiles.json',import.meta.url))):null;
const fixture=profile?{...profiles,...profiles.certificates[profile.certificate??'valid']}:JSON.parse(readFileSync(new URL('./hive-push-tls.json',import.meta.url)));
const require=createRequire(process.env.IVY_HIVE_TEST_WEB_PUSH);
const webPush=require(process.env.IVY_HIVE_TEST_WEB_PUSH);
const vapid=require('./vapid-helper.js');
const ece=require('http_ece');
const scalar=n=>{const bytes=Buffer.alloc(32);bytes[31]=n;return bytes;};
const receiver=crypto.createECDH('prime256v1');receiver.setPrivateKey(scalar(1));
const signer=crypto.createECDH('prime256v1');signer.setPrivateKey(scalar(3));
const details={subject:'mailto:push@example.test',publicKey:signer.getPublicKey().toString('base64url'),privateKey:scalar(3).toString('base64url')};
const sockets=new Map(),requests=[],completions=[],timers=new Set(),lookups=[];let connectionId=0;
const later=(action,delay)=>{const timer=setTimeout(()=>{timers.delete(timer);action();},delay);timers.add(timer);};
const server=https.createServer({key:fixture.key,cert:fixture.cert,minVersion:profile?.minVersion??'TLSv1.2',...(profile?.maxVersion?{maxVersion:profile.maxVersion}:{})},(req,res)=>{
 let body=[];
 req.on('data',chunk=>body.push(chunk));
 req.on('end',()=>{
  const bytes=Buffer.concat(body);
  const token=req.headers.authorization?.slice('vapid t='.length).split(', k=')[0].split('.');
  const advertised=signer.getPublicKey();
  const publicKey=crypto.createPublicKey({format:'jwk',key:{kty:'EC',crv:'P-256',x:advertised.subarray(1,33).toString('base64url'),y:advertised.subarray(33).toString('base64url')}});
  const signatureValid=!!token&&crypto.verify('sha256',Buffer.from(token.slice(0,2).join('.')),{key:publicKey,dsaEncoding:'ieee-p1363'},Buffer.from(token[2],'base64url'));
  const plaintext=bytes.length?ece.decrypt(bytes,{version:'aes128gcm',privateKey:receiver,authSecret:Buffer.alloc(16,7).toString('base64url')}).toString('utf8'):'';
  requests.push({connection:sockets.get(req.socket),method:req.method,path:req.url,rawHeaders:req.rawHeaders,body:bytes.toString('base64'),tls:req.socket.getProtocol(),cipher:req.socket.getCipher(),signatureValid,plaintext,claims:token?JSON.parse(Buffer.from(token[1],'base64url')):null,servername:req.socket.servername,sessionReused:req.socket.isSessionReused(),remoteAddress:req.socket.remoteAddress});
  res.on('finish',()=>completions.push({path:req.url}));
  const path=req.url.split('?')[0];
  if(path.startsWith('/raw/')){
   const kind=path.slice('/raw/'.length);
   const raw={
    normal:'HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok',
    folded:'HTTP/1.1 201 Created\r\nX-Test: one\r\n two\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok',
    space:'HTTP/1.1 201 Created\r\nX-Test : value\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok',
    lf:'HTTP/1.1 201 Created\nContent-Length: 2\nConnection: close\n\nok',
    duplicate:'HTTP/1.1 201 Created\r\nContent-Length: 2\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok',
    conflict:'HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\nConnection: close\r\n\r\n0\r\n\r\n',
    chunked:'HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nok\r\n0\r\nX-End: done\r\n\r\n',
    interim:'HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok',
    old:'HTTP/1.0 201 Created\r\nContent-Length: 2\r\n\r\nok',
    many:'HTTP/1.1 201 Created\r\n'+'X:a\r\n'.repeat(2200)+'Content-Length: 2\r\nConnection: close\r\n\r\nok',
    large:'HTTP/1.1 201 Created\r\nX: '+'a'.repeat(17000)+'\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok',
   }[kind];
   if(!raw)throw Error('Unknown raw response');
   req.socket.end(raw);return;
  }
  if(path==='/drop'){req.socket.destroy();return;}
  if(path==='/stall-headers'||path==='/trickle-headers'){
   req.socket.write('HTTP/1.1 201 Created\r\nX-Test: ');
   if(path==='/trickle-headers'){
    later(()=>req.socket.write('partial'),4000);
    later(()=>req.socket.write('-header'),8000);
    later(()=>req.socket.end('\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok'),12000);
   }
   return;
  }
  const status=path.startsWith('/status/')?Number(path.split('/').at(-1)):path==='/redirect'?302:200;
  const headers={Connection:path==='/close'?'close':'keep-alive','Keep-Alive':path==='/short-idle'?'timeout=1':'timeout=5'};
  if(path==='/redirect')headers.Location='/status/200';
  if(path==='/compressed')headers['Content-Encoding']='gzip';
  res.writeHead(status,headers);
  if(path==='/stall-body'){res.write('partial');return;}
  if(path==='/delay-body'){res.flushHeaders();later(()=>res.end('ok'),600);return;}
  if(path==='/trickle-body'){
   res.write('a');later(()=>res.write('b'),4000);later(()=>res.write('c'),8000);later(()=>res.end('d'),12000);return;
  }
  res.end(path==='/compressed'?'deliberately not a gzip stream':'ok');
 });
});
server.on('secureConnection',socket=>{sockets.set(socket,++connectionId);socket.on('close',()=>sockets.delete(socket));});
server.on('tlsClientError',()=>{});
await new Promise(resolve=>server.listen(0,profile?.dualStack?'::':'127.0.0.1',resolve));
const port=server.address().port;
const base=`https://localhost:${port}`;
const lookup=profile?.addresses?{autoSelectFamily:profile.autoSelectFamily??true,autoSelectFamilyAttemptTimeout:profile.attemptTimeout??250,lookup(host,options,callback){
 lookups.push({host,all:options.all??false,family:options.family,hints:options.hints});
 setTimeout(()=>{const addresses=profile.addresses.map(address=>({address,family:address.includes(':')?6:4}));if(options.all)callback(null,addresses);else callback(null,addresses[0].address,addresses[0].family);},profile.lookupDelay??0);
}}:{};
const agent=new https.Agent({ca:fixture.ca,keepAlive:true,scheduling:'lifo',timeout:5000,...lookup});
const connectionEvents=[];
const createConnection=agent.createConnection;
agent.createConnection=function(...args){
 const socket=createConnection.apply(this,args);
 for(const event of ['connectionAttempt','connectionAttemptFailed','connectionAttemptTimeout'])socket.on(event,(address,port,family,error)=>connectionEvents.push({event,address,port,family,error:error?.code??null}));
 return socket;
};
const untrusted=new https.Agent({ca:[],keepAlive:true,scheduling:'lifo',timeout:5000});
console.log(JSON.stringify({port,base,caDer:fixture.caDer,profile,details,subscription:{endpoint:base+'/status/201',keys:{p256dh:receiver.getPublicKey().toString('base64url'),auth:Buffer.alloc(16,7).toString('base64url')}}}));
for await(const line of createInterface({input:process.stdin,crlfDelay:Infinity})){
 const input=JSON.parse(line);
 if(input.op==='original'){
  const before=requests.length,start=performance.now();let status,errorCode=null;
  try{const result=await webPush.sendNotification({endpoint:(input.hostname?`https://${input.hostname}:${port}`:input.host==='ip'?`https://127.0.0.1:${port}`:base)+input.path,keys:{p256dh:receiver.getPublicKey().toString('base64url'),auth:Buffer.alloc(16,7).toString('base64url')}},input.payload??'notification',{TTL:3600,timeout:10000,vapidDetails:details,agent:input.trusted===false?untrusted:agent});status=result.statusCode;}
  catch(error){status=error.statusCode??0;errorCode=error.code??error.message??null;}
  console.log(JSON.stringify({status,millis:performance.now()-start,requests:requests.slice(before),lookups,connectionEvents,errorCode}));
 }else if(input.op==='environment'){
  let proxyRequests=0;
  const proxySockets=new Set();
  const proxy=net.createServer(socket=>{
   proxySockets.add(socket);socket.on('error',()=>{});socket.on('close',()=>proxySockets.delete(socket));
   socket.once('data',data=>{
    if(!data.toString().startsWith(`CONNECT localhost:${port} `)){socket.destroy();return;}
    ++proxyRequests;
    const upstream=net.connect(port,'127.0.0.1',()=>{socket.write('HTTP/1.1 200 Connection Established\r\n\r\n');socket.pipe(upstream).pipe(socket);});
    proxySockets.add(upstream);upstream.on('error',()=>socket.destroy());upstream.on('close',()=>proxySockets.delete(upstream));
   });
  });
  await new Promise(resolve=>proxy.listen(0,'127.0.0.1',resolve));
  const flags=[...(input.envProxy?['--use-env-proxy']:[]),...(input.systemCA?['--use-system-ca']:[])];
  const child=spawn(process.execPath,[...flags,fileURLToPath(new URL('./hive-push-environment-oracle.mjs',import.meta.url))],{windowsHide:true,stdio:['pipe','pipe','pipe'],env:{...process.env,NODE_OPTIONS:'',NODE_EXTRA_CA_CERTS:input.extraCA?process.env.IVY_HIVE_PUSH_PROFILE_CA_FILE:'',NODE_USE_SYSTEM_CA:'',NODE_TLS_REJECT_UNAUTHORIZED:input.insecure?'0':'1',SSL_CERT_FILE:'',SSL_CERT_DIR:'',HTTPS_PROXY:input.envProxy||input.ambientProxy?`http://127.0.0.1:${proxy.address().port}`:'',HTTP_PROXY:'',ALL_PROXY:'',NO_PROXY:''}});
  let output='',errors='';child.stdout.on('data',data=>output+=data);child.stderr.on('data',data=>errors+=data);
  child.stdin.end(JSON.stringify({subscription:{endpoint:base+'/status/201',keys:{p256dh:receiver.getPublicKey().toString('base64url'),auth:Buffer.alloc(16,7).toString('base64url')}},details}));
  const code=await new Promise((resolve,reject)=>{child.on('error',reject);child.on('close',resolve);});
  for(const socket of proxySockets)socket.destroy();proxy.closeAllConnections?.();await new Promise(resolve=>proxy.close(resolve));
  console.log(JSON.stringify({code,result:code===0?JSON.parse(output):null,stderr:errors,proxyRequests}));
 }else if(input.op==='expiration')console.log(JSON.stringify({expiration:vapid.getFutureExpirationTimestamp(12*60*60)}));
 else if(input.op==='stats')console.log(JSON.stringify({requests,completions,sockets:sockets.size}));
 else if(input.op==='close'){
  for(const timer of timers)clearTimeout(timer);timers.clear();
  agent.destroy();untrusted.destroy();for(const socket of sockets.keys())socket.destroy();
  server.closeAllConnections();await new Promise(resolve=>server.close(resolve));console.log(JSON.stringify({closed:true}));break;
 }else throw Error('Unknown fixture operation');
}
// Caller failure/EOF also retires this isolated peer.
for(const timer of timers)clearTimeout(timer);
agent.destroy();untrusted.destroy();for(const socket of sockets.keys())socket.destroy();server.closeAllConnections();server.close();
