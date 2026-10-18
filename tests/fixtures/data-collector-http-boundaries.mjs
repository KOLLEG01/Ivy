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
    const add=(name,fragments,extra={})=>cases.push({name:kind+'-'+name,kind,raw:fragments,...extra});
    if(mode==='timeouts') {
      add('header-timeout',[{afterMs:0,data:start+'Authorization:'}]);
      add('body-timeout',[{afterMs:0,data:start+auth+end.slice(0,-body.length)+body.slice(0,1)}]);
      add('header-after-11s',[{afterMs:0,data:start},{afterMs:11000,data:auth+end}]);
      add('body-after-16s',[{afterMs:0,data:start+auth+end.slice(0,-body.length)},{afterMs:16000,data:body}]);
      continue;
    }
    add('normal',[{afterMs:0,data:start+auth+end}]);
    for(const size of [16180,16200,16220,16240,16248,16249,16260,16270,16273,16274,16275,16280,16300,16320,16384]) {
      add('header-padding-'+size,[{afterMs:0,data:start+auth+'X-Pad: '+'x'.repeat(size)+'\r\n'+end}]);
    }
    add('leading-value-whitespace',[{afterMs:0,data:start+auth+'X-Pad: '+' '.repeat(70000)+'ok\r\n'+end}]);
    add('trailing-value-whitespace-budget',[{afterMs:0,data:start+auth+'X-Pad: '+'x'.repeat(16270)+' '.repeat(8)+'\r\n'+end}]);
    add('idempotency-duplicate-after-prefix',[{afterMs:0,data:start+auth.replace('run','first')+'X: a\r\n'.repeat(996)+'Idempotency-Key: second\r\n'+end}]);
    add('late-connection-close',[{afterMs:0,data:start.replace('Connection: close\r\n','')+auth+'X: a\r\n'.repeat(997)+'Connection: close\r\n'+end}]);
    const chunked=Buffer.byteLength(body).toString(16)+'\r\n'+body+'\r\n0\r\nX-Trail: ok\r\n\r\n';
    add('chunked-framing-after-prefix',[{afterMs:0,data:start+auth+'X: a\r\n'.repeat(1000)+'Transfer-Encoding: chunked\r\n\r\n'+chunked}]);
    const first=start.replace('Connection: close','Connection: keep-alive')+auth.replace('run','first');
    const second=start+auth.replace('run','second')+end;
    add('pipelined-fixed',[{afterMs:0,data:first+end+second}],{pipeline:true});
    add('pipelined-chunked',[{afterMs:0,data:first+'Transfer-Encoding: chunked\r\n\r\n'+chunked+second}],{pipeline:true});
    add('fragmented-head',[{afterMs:0,data:start.slice(0,9)},{afterMs:20,data:start.slice(9)+auth.slice(0,7)},{afterMs:20,data:auth.slice(7)+end}]);
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

export function response(bytes,pipeline=false) {
  let offset=0;
  const frames=[];
  while(offset<bytes.length) {
    const split=bytes.indexOf('\r\n\r\n',offset);
    if(split<0)throw Error('Truncated HTTP response');
    const headers=bytes.subarray(offset,split).toString('latin1');
    const status=Number(/^HTTP\/1\.[01] (\d+)/.exec(headers)?.[1]);
    offset=split+4;
    if(status>=100&&status<200)continue;
    let body;
    if(/transfer-encoding: chunked/i.test(headers)) {
      const chunks=[];
      for(;;) {
        const line=bytes.indexOf('\r\n',offset);
        const count=Number.parseInt(bytes.subarray(offset,line).toString('ascii'),16);
        offset=line+2;
        if(!count) {while(bytes.indexOf('\r\n',offset)!==offset)offset=bytes.indexOf('\r\n',offset)+2;offset+=2;break;}
        chunks.push(bytes.subarray(offset,offset+count));offset+=count+2;
      }
      body=Buffer.concat(chunks);
    } else {
      const length=/\r\ncontent-length:\s*(\d+)/i.exec('\r\n'+headers)?.[1];
      const end=length===undefined?bytes.length:offset+Number(length);
      body=bytes.subarray(offset,end);offset=end;
    }
    frames.push({status,body:body.length?JSON.parse(body.toString('utf8')):null});
  }
  return pipeline?{responses:frames}:(frames[0]??{status:null,body:null});
}

export async function sendBoundary(port,c,token) {
  const started=performance.now();
  const socket=createConnection({host:'127.0.0.1',port});
  let timer;
  const result=new Promise((resolve,reject)=>{
    const bytes=[];
    socket.on('data',chunk=>bytes.push(chunk));
    socket.on('error',error=>{if(error.code!=='ECONNRESET')reject(error);});
    socket.on('close',()=>{clearTimeout(timer);try{resolve({response:response(Buffer.concat(bytes),!!c.pipeline),elapsedMs:performance.now()-started});}catch(error){reject(error);}});
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
