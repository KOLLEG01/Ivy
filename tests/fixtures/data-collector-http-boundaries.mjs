import {createConnection} from 'node:net';
import {performance} from 'node:perf_hooks';
import {setTimeout as delay} from 'node:timers/promises';

export function boundaryCases(mode) {
  const cases=[];
  for(const kind of ['ingress','mcp']) {
    const path=kind==='ingress'?'/tasks/task/run':'/call';
    const token=kind==='ingress'?'synthetic':'TOKEN';
    const body=kind==='ingress'?'{}':'{"tool":"echo"}';
    const start=`POST ${path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n`;
    const auth=`Authorization: Bearer ${token}\r\nIdempotency-Key: run\r\n`;
    const end=`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;
    const add=(name,fragments)=>cases.push({name:kind+'-'+name,kind,raw:fragments});
    if(mode==='timeouts') {
      add('header-timeout',[{afterMs:0,data:start+'Authorization:'}]);
      add('body-timeout',[{afterMs:0,data:start+auth+end.slice(0,-body.length)+body.slice(0,1)}]);
      add('header-after-11s',[{afterMs:0,data:start},{afterMs:11000,data:auth+end}]);
      add('body-after-16s',[{afterMs:0,data:start+auth+end.slice(0,-body.length)},{afterMs:16000,data:body}]);
      continue;
    }
    add('normal',[{afterMs:0,data:start+auth+end}]);
    for(const size of [16180,16200,16220,16240,16260,16270,16274,16275,16280,16300,16320,16384]) {
      add('header-padding-'+size,[{afterMs:0,data:start+auth+'X-Pad: '+'x'.repeat(size)+'\r\n'+end}]);
    }
    for(const count of [990,995,996,997,998,999,1000,1001,1990,2000,2100]) {
      add('early-auth-headers-'+count,[{afterMs:0,data:start+auth+'X: a\r\n'.repeat(count)+end}]);
      add('late-auth-headers-'+count,[{afterMs:0,data:start+'X: a\r\n'.repeat(count)+auth+end}]);
    }
    for(const size of [16200,16260,16300,16360,16384]) {
      add('request-target-'+size,[{afterMs:0,data:`POST /${'x'.repeat(size)} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n${auth}${end}`}]);
    }
  }
  return cases;
}

export function response(bytes) {
  const text=bytes.toString('utf8');
  let offset=0,status=null,body=null;
  for(;;) {
    const split=text.indexOf('\r\n\r\n',offset);
    if(split<0)break;
    const headers=text.slice(offset,split);
    status=Number(/^HTTP\/1\.[01] (\d+)/.exec(headers)?.[1]);
    offset=split+4;
    if(status>=100&&status<200)continue;
    let value=text.slice(offset);
    if(/transfer-encoding: chunked/i.test(headers)) {
      let decoded='';
      for(;;) {
        const line=value.indexOf('\r\n');
        const count=Number.parseInt(value.slice(0,line),16);
        if(!count)break;
        decoded+=value.slice(line+2,line+2+count);
        value=value.slice(line+2+count+2);
      }
      value=decoded;
    }
    body=value?JSON.parse(value):null;
    break;
  }
  return {status,body};
}

export async function sendBoundary(port,c,token) {
  const started=performance.now();
  const socket=createConnection({host:'127.0.0.1',port});
  let timer;
  const result=new Promise((resolve,reject)=>{
    const bytes=[];
    socket.on('data',chunk=>bytes.push(chunk));
    socket.on('error',error=>{if(error.code!=='ECONNRESET')reject(error);});
    socket.on('close',()=>{clearTimeout(timer);try{resolve({response:response(Buffer.concat(bytes)),elapsedMs:performance.now()-started});}catch(error){reject(error);}});
    timer=setTimeout(()=>{socket.destroy();reject(Error('Original HTTP boundary observation exceeded 42 seconds: '+c.name));},42000);
  });
  socket.once('connect',()=>{void(async()=>{
    for(const part of c.raw) {
      if(part.afterMs)await delay(part.afterMs);
      if(socket.destroyed)break;
      socket.write(part.data.replaceAll('TOKEN',token??'TOKEN'));
    }
  })().catch(error=>socket.destroy(error));});
  return result;
}
