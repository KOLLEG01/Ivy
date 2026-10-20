// Original send policy on a real ws/TCP owner. The isolated handshake only
// obtains an upgraded stream; it does not qualify Hive HTTP or WS ingress.
import {registerHooks,stripTypeScriptTypes,createRequire} from 'node:module';
import {readFileSync,existsSync,writeFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createServer} from 'node:http';
import {connect} from 'node:net';
import {createHash} from 'node:crypto';
import {join} from 'node:path';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}return next(specifier,context);},load(url,context,next){if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};return next(url,context);}});
const require=createRequire(import.meta.url),{WebSocketServer,WebSocket}=require(process.env.IVY_TEST_WS_MODULE??'ws');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {encodeJson}=await import('../../packages/contracts/src/canonical-json.ts');
const {managementFrameBytes,managementSocketBufferBytes}=await import('../../packages/contracts/src/limits.ts');
const serverSource=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8'),kernelSource=readFileSync(new URL('../../services/hive/src/kernel.ts',import.meta.url),'utf8');
function span(text,start,end){const a=text.indexOf(start),b=text.indexOf(end,a+start.length);if(a<0||b<0)throw Error('Original source boundary changed');return text.slice(a,b);}
const rpc=new Function('IvyError',stripTypeScriptTypes(span(kernelSource,'export function rpcFailure(', '\nexport function validRequestId(').replace('export ', '')+span(kernelSource,'export function validRequestId(', '\n}').concat('\n}').replace('export ',''),{mode:'transform'})+';return {rpcFailure,validRequestId};')(IvyError);
const Original=new Function('IvyError','WebSocket','encodeJson','rpcFailure','validRequestId','managementSocketBufferBytes',stripTypeScriptTypes('class Original {\n'+span(serverSource,'  private send(', '  private connected(')+'\n}',{mode:'transform'})+';return Original;')(IvyError,WebSocket,encodeJson,rpc.rpcFailure,rpc.validRequestId,managementSocketBufferBytes);
const sleep=ms=>new Promise(yes=>setTimeout(yes,ms));
async function until(test){for(let n=0;n<1000;n++){if(test())return;await sleep(2);}throw Error('Socket fixture deadline exceeded');}
const hash=bytes=>createHash('sha256').update(bytes).digest('hex'),results=[];
for(const input of JSON.parse(readFileSync(0,'utf8'))){
 if(!/^[a-z0-9-]+$/.test(input.name))throw Error('Invalid fixture name');
 const http=createServer(),wss=new WebSocketServer({noServer:true,maxPayload:managementFrameBytes,perMessageDeflate:false}),chunks=[],snapshots=[],unhandled=[];let ws,closed=0,wireBytes=0,clientClosed=false,headers=Buffer.alloc(0),headerDone=false;
 const onUnhandled=e=>unhandled.push(IvyError.from(e).toWire());process.on('uncaughtException',onUnhandled);
 http.on('upgrade',(req,socket,head)=>wss.handleUpgrade(req,socket,head,value=>{ws=value;ws.on('error',()=>undefined);ws.on('close',()=>{closed++;if(input.closeError)throw new IvyError('fixture_close_error','Close callback failed.','unknown');});}));
 await new Promise(yes=>http.listen(0,'127.0.0.1',yes));
 const client=connect(http.address().port,'127.0.0.1');client.on('error',()=>undefined);client.on('close',()=>clientClosed=true);
 client.on('data',bytes=>{if(!headerDone){headers=Buffer.concat([headers,bytes]);const at=headers.indexOf('\r\n\r\n');if(at<0)return;if(!headers.subarray(0,at).toString('ascii').startsWith('HTTP/1.1 101 '))throw Error('Fixture handshake failed');bytes=headers.subarray(at+4);headerDone=true;headers=Buffer.alloc(0);}if(bytes.length){chunks.push(bytes);wireBytes+=bytes.length;}});
 await new Promise(yes=>client.once('connect',yes));client.write('GET /fixture HTTP/1.1\r\nHost: example.test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n');
 await until(()=>ws&&headerDone);if(input.pause)client.pause();
 const owner=new Original(),peer={socket:ws};let observer=ws;
 try{
  for(const step of input.steps){
   if(step.op==='send'){const encoded=step.encodedLength===undefined?step.encoded:('"'+'x'.repeat(step.encodedLength-2)+'"');owner.send(peer,step.value??null,encoded);}
   else if(step.op==='ping')ws.ping();
   else if(step.op==='raw')ws._socket.write(Buffer.from(step.base64,'base64'));
   else if(step.op==='snapshot')snapshots.push({name:step.name,open:ws.readyState===WebSocket.OPEN,buffered:ws.bufferedAmount,closed,unhandled:structuredClone(unhandled)});
   else if(step.op==='resume'){client.resume();await until(()=>wireBytes>=step.bytes);await until(()=>ws.bufferedAmount===0);}
   else if(step.op==='drop-observer')observer=null;
   else if(step.op==='terminate')ws.terminate();
   else if(step.op==='client-end')client.end();
   else if(step.op==='client-reset'){client.resetAndDestroy();Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,25);}
   else if(step.op==='settle-close'){client.resume();await until(()=>closed===1&&clientClosed);await new Promise(yes=>setImmediate(yes));}
   else throw Error('Unknown socket step '+step.op);
  }
  const bytes=Buffer.concat(chunks);if(process.env.IVY_HIVE_WS_WIRES)writeFileSync(join(process.env.IVY_HIVE_WS_WIRES,input.name+'-original.bin'),bytes);
  results.push({name:input.name,snapshots,wire:{byteLength:bytes.length,sha256:hash(bytes)},closed,unhandled});
 }finally{ws.terminate();client.destroy();http.closeAllConnections();await new Promise(yes=>http.close(yes));wss.close();process.off('uncaughtException',onUnhandled);}
}
process.stdout.write(JSON.stringify(results));
