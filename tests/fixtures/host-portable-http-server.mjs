import http from 'node:http';
import https from 'node:https';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {gzipSync,deflateSync,deflateRawSync,brotliCompressSync,zstdCompressSync} from 'node:zlib';

// This key is public test data, trusted only by the isolated clients below.
const key=readFileSync(new URL('./host-portable-http-key.pem',import.meta.url));
const cert=readFileSync(new URL('./host-portable-http-cert.pem',import.meta.url));
const plain=Buffer.from('portable native bytes\0\xff','latin1');
const gzip=gzipSync(plain),brotli=brotliCompressSync(plain),zstd=zstdCompressSync(plain);
const badCrc=Buffer.from(gzip);badCrc[badCrc.length-8]^=1;
const damagedZstd=Buffer.from(zstd);damagedZstd[0]^=1;
const nestedBadCrc=gzipSync(gzip);nestedBadCrc[nestedBadCrc.length-8]^=1;
const chain=body=>brotliCompressSync(gzipSync(body));
const cases=new Map([
  ['plain',{body:plain}],['gzip',{body:gzip,coding:'gzip'}],['x-gzip',{body:gzip,coding:'x-gzip'}],
  ['upper',{body:gzip,coding:'GZiP'}],['zlib',{body:deflateSync(plain),coding:'deflate'}],
  ['raw',{body:deflateRawSync(plain),coding:'deflate'}],['br',{body:brotli,coding:'br'}],['zstd',{body:zstd,coding:'zstd'}],
  ['chain',{body:chain(plain),coding:'gzip, br'}],['duplicates',{body:chain(plain),coding:['gzip','br']}],
  ['five',{body:gzipSync(chain(chain(plain))),coding:'gzip, br, gzip, br, gzip'}],
  ['six',{body:plain,coding:'gzip,gzip,gzip,gzip,gzip,gzip'}],
  ['six-error',{body:plain,coding:'unknown,unknown,unknown,unknown,unknown,unknown',status:503}],
  ['unknown',{body:plain,coding:'future-coding'}],['mixed-unknown',{body:plain,coding:'gzip, future-coding, br'}],
  ['identity',{body:plain,coding:'identity'}],['empty',{body:Buffer.alloc(0),coding:'gzip'}],
  ['truncated-gzip',{body:gzip.subarray(0,-5),coding:'gzip'}],['gzip-header',{body:gzip.subarray(0,4),coding:'gzip'}],
  ['truncated-zlib',{body:deflateSync(plain).subarray(0,-3),coding:'deflate'}],
  ['truncated-raw',{body:deflateRawSync(plain).subarray(0,-1),coding:'deflate'}],
  ['truncated-br',{body:brotli.subarray(0,-1),coding:'br'}],['truncated-zstd',{body:zstd.subarray(0,-1),coding:'zstd'}],
  ['bad-crc',{body:badCrc,coding:'gzip'}],['bad-zstd',{body:damagedZstd,coding:'zstd'}],
  ['gzip-zero-padding',{body:Buffer.concat([gzip,Buffer.alloc(9)]),coding:'gzip'}],
  ['gzip-zero-garbage',{body:Buffer.concat([gzip,Buffer.from([0]),Buffer.from('garbage')]),coding:'gzip'}],
  ['nested-bad-crc',{body:nestedBadCrc,coding:'gzip,gzip',split:true}],
  ['gzip-concat',{body:Buffer.concat([gzip,gzip]),coding:'gzip'}],
  ['gzip-garbage',{body:Buffer.concat([gzip,Buffer.from('garbage')]),coding:'gzip'}],
  ['zstd-concat',{body:Buffer.concat([zstd,zstd]),coding:'zstd'}],
  ['status',{body:plain,status:503}],['no-content',{body:Buffer.alloc(0),status:204,coding:'gzip,gzip,gzip,gzip,gzip,gzip'}],
  ['large-declared',{body:plain,declared:64*1024*1024+1}],
  ['split-gzip',{body:gzip,coding:'gzip',split:true}],['split-raw',{body:deflateRawSync(plain),coding:'deflate',split:true}],
  // Keep these TLS sockets open while the decoder drains. Node's parser aborts
  // the process if a TLS EOF arrives while its compressed body reader is paused.
  ['large',{body:gzipSync(Buffer.alloc(64*1024*1024,97)),coding:'gzip',keepAlive:true}],
  ['oversize',{body:gzipSync(Buffer.alloc(64*1024*1024+1,97)),coding:'gzip',keepAlive:true}],
  ['late-close',{body:gzipSync(Buffer.alloc(64*1024*1024+1,97)),coding:'gzip',destroy:true,destroyDelay:1000}],
  ['late-zstd',{body:zstd,coding:'zstd',destroy:true}],['late-br',{body:brotli,coding:'br',destroy:true}],
]);
if(process.argv[2])cases.set('archive',{body:readFileSync(process.argv[2])});
const requests=[],owners={};let active=0,maximum=0;
let httpRoot,httpsRoot;
function handle(req,res){
  const url=new URL(req.url,'http://localhost'),parts=url.pathname.split('/').filter(Boolean);
  if(parts[0]==='stats'){
    const body=JSON.stringify({requests,active,maximum,owners});res.setHeader('Content-Length',Buffer.byteLength(body));res.end(body);return;
  }
  const [actor,kind,argument]=parts;
  active++;maximum=Math.max(maximum,active);
  const owner=owners[actor]??={active:0,maximum:0};owner.active++;owner.maximum=Math.max(owner.maximum,owner.active);
  const record={actor,kind,argument:argument??null,headers:req.headers};requests.push(record);
  let closed=false;const finish=()=>{if(!closed){closed=true;active--;owner.active--;}};
  res.on('close',finish);res.on('finish',finish);
  res.setHeader('Connection','close');
  if(kind==='redirect'){
    if(+argument){res.writeHead(302,{Location:`/${actor}/redirect/${+argument-1}`});res.end();return;}
  }
  if(kind==='relative'||kind==='downgrade'||kind==='credential'||kind==='bad-location'||kind==='duplicate-location'){
    const location=kind==='relative'?`./plain?escaped=%C3%BC`:kind==='downgrade'?`${httpRoot}/${actor}/plain`:
      kind==='credential'?`${httpsRoot.replace('://','://user:password@')}/${actor}/plain`:
      kind==='duplicate-location'?[`/${actor}/plain`,`/${actor}/plain`]:'file:///unreachable';
    res.writeHead(302,{Location:location});res.end();return;
  }
  if(kind==='hang'){res.writeHead(200);res.write(plain);return;}
  const selected=kind==='redirect'?'plain':kind==='worker'?(argument==='fail'?'status':'archive'):kind;
  const value=cases.get(selected)??{body:plain,status:404};
  if(value.keepAlive)res.setHeader('Connection','keep-alive');
  const send=()=>{
    res.statusCode=value.status??200;
    if(value.coding)res.setHeader('Content-Encoding',value.coding);
    if(!value.split&&!value.destroy)res.setHeader('Content-Length',value.declared??value.body.length);
    if(value.destroy){res.write(value.body);setTimeout(()=>res.destroy(),value.destroyDelay??25).unref();return;}
    if(value.split){let at=0;const part=()=>{if(res.destroyed)return;if(at===value.body.length){res.end();return;}res.write(value.body.subarray(at,++at));setImmediate(part);};part();}
    else res.end(value.body);
  };
  if(kind==='worker')setTimeout(send,argument==='fail'?80:300);else send();
}
const insecure=http.createServer(handle),secure=https.createServer({key,cert},handle);
await Promise.all([new Promise(resolve=>insecure.listen(0,'127.0.0.1',resolve)),new Promise(resolve=>secure.listen(0,'127.0.0.1',resolve))]);
httpRoot=`http://127.0.0.1:${insecure.address().port}`;httpsRoot=`https://127.0.0.1:${secure.address().port}`;
console.log(JSON.stringify({http:httpRoot,https:httpsRoot,plain:plain.toString('base64'),node:process.versions.node,undici:process.versions.undici,
  fixtures:[...cases].map(([name,value])=>({name,encoding:value.coding??null,status:value.status??200,declared:value.declared??null,bytes:value.body.length,sha256:createHash('sha256').update(value.body).digest('hex')}))}));
