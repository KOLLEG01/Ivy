// Isolated HTTPS peer and the pinned original web-push sender. No live provider.
import https from 'node:https';
import {createRequire} from 'node:module';
import {readFileSync} from 'node:fs';
import {createInterface} from 'node:readline';
import crypto from 'node:crypto';

const fixture=JSON.parse(readFileSync(new URL('./hive-push-tls.json',import.meta.url)));
const require=createRequire(process.env.IVY_HIVE_TEST_WEB_PUSH);
const webPush=require(process.env.IVY_HIVE_TEST_WEB_PUSH);
const vapid=require('./vapid-helper.js');
const ece=require('http_ece');
const scalar=n=>{const bytes=Buffer.alloc(32);bytes[31]=n;return bytes;};
const receiver=crypto.createECDH('prime256v1');receiver.setPrivateKey(scalar(1));
const signer=crypto.createECDH('prime256v1');signer.setPrivateKey(scalar(3));
const details={subject:'mailto:push@example.test',publicKey:signer.getPublicKey().toString('base64url'),privateKey:scalar(3).toString('base64url')};
const sockets=new Map(),requests=[],completions=[],timers=new Set();let connectionId=0;
const later=(action,delay)=>{const timer=setTimeout(()=>{timers.delete(timer);action();},delay);timers.add(timer);};
const server=https.createServer({key:fixture.key,cert:fixture.cert,minVersion:'TLSv1.2'},(req,res)=>{
 let body=[];
 req.on('data',chunk=>body.push(chunk));
 req.on('end',()=>{
  const bytes=Buffer.concat(body);
  const token=req.headers.authorization?.slice('vapid t='.length).split(', k=')[0].split('.');
  const advertised=signer.getPublicKey();
  const publicKey=crypto.createPublicKey({format:'jwk',key:{kty:'EC',crv:'P-256',x:advertised.subarray(1,33).toString('base64url'),y:advertised.subarray(33).toString('base64url')}});
  const signatureValid=!!token&&crypto.verify('sha256',Buffer.from(token.slice(0,2).join('.')),{key:publicKey,dsaEncoding:'ieee-p1363'},Buffer.from(token[2],'base64url'));
  const plaintext=bytes.length?ece.decrypt(bytes,{version:'aes128gcm',privateKey:receiver,authSecret:Buffer.alloc(16,7).toString('base64url')}).toString('utf8'):'';
  requests.push({connection:sockets.get(req.socket),method:req.method,path:req.url,rawHeaders:req.rawHeaders,body:bytes.toString('base64'),tls:req.socket.getProtocol(),signatureValid,plaintext,claims:token?JSON.parse(Buffer.from(token[1],'base64url')):null});
  res.on('finish',()=>completions.push({path:req.url}));
  const path=req.url.split('?')[0];
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
  const headers={Connection:'keep-alive','Keep-Alive':path==='/short-idle'?'timeout=1':'timeout=5'};
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
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const port=server.address().port;
const base=`https://localhost:${port}`;
const agent=new https.Agent({ca:fixture.ca,keepAlive:true,scheduling:'lifo',timeout:5000});
const untrusted=new https.Agent({ca:[],keepAlive:true,scheduling:'lifo',timeout:5000});
console.log(JSON.stringify({port,base,caDer:fixture.caDer,details,subscription:{endpoint:base+'/status/201',keys:{p256dh:receiver.getPublicKey().toString('base64url'),auth:Buffer.alloc(16,7).toString('base64url')}}}));
for await(const line of createInterface({input:process.stdin,crlfDelay:Infinity})){
 const input=JSON.parse(line);
 if(input.op==='original'){
  const before=requests.length,start=performance.now();let status;
  try{const result=await webPush.sendNotification({endpoint:(input.host==='ip'?`https://127.0.0.1:${port}`:base)+input.path,keys:{p256dh:receiver.getPublicKey().toString('base64url'),auth:Buffer.alloc(16,7).toString('base64url')}},input.payload??'notification',{TTL:3600,timeout:10000,vapidDetails:details,agent:input.trusted===false?untrusted:agent});status=result.statusCode;}
  catch(error){status=error.statusCode??0;}
  console.log(JSON.stringify({status,millis:performance.now()-start,requests:requests.slice(before)}));
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
