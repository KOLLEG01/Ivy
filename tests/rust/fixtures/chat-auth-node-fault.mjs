// Fault injection for the isolated auth child, not part of the runtime adapter.
import {appendFileSync,existsSync,writeFileSync} from 'node:fs';
import {setTimeout as delay} from 'node:timers/promises';
const mode=process.env.IVY_CHAT_AUTH_FAULT, gate=process.env.IVY_CHAT_AUTH_GATE;
const originalWrite=process.stdout.write.bind(process.stdout);
const retirement=mode?.startsWith('retire-');
function trace(value) { if(retirement)appendFileSync(gate+'.trace',JSON.stringify(value)+'\n'); }
async function released() { while(!existsSync(gate+'.release'))await delay(10); }
trace({event:'started',pid:process.pid});
// Keep the actual child/tree alive after its ACK while stderr is drained.
const alive=mode==='retire-exit-gate'?setInterval(()=>{},1000):null;
function packet(frame) {
  const bytes=Buffer.from(JSON.stringify(frame)), header=Buffer.alloc(4);
  header.writeUInt32BE(bytes.length); return Buffer.concat([header,bytes]);
}
process.stdout.write=function(bytes,...args) {
  if (Buffer.isBuffer(bytes) && bytes.length>=4 && bytes.readUInt32BE()===bytes.length-4) {
    const frame=JSON.parse(bytes.subarray(4));
    if (retirement && frame.p===0 && frame.e) {
      const body=JSON.parse(Buffer.from(frame.b,'base64'));
      if(body.kind==='stopped') {
        trace({event:'stopped',request:frame.r,operationId:body.operationId});
        if(mode==='retire-stop-loss')process.exit(75);
        if(mode==='retire-stop-gate') {
          writeFileSync(gate+'.observed','stop-ack-withheld');
          void released().then(()=>originalWrite(bytes,...args));
          return true;
        }
        if(mode==='retire-exit-gate') {
          process.stderr.write(Buffer.alloc(256*1024,0x64));
          writeFileSync(gate+'.observed','stop-ack-sent-child-live');
          void released().then(()=>clearInterval(alive));
        }
      }
    }
    if (frame.r==='2' && frame.p===0 && frame.e) {
      const body=JSON.parse(Buffer.from(frame.b,'base64'));
      if (body.kind==='set') {
        if (mode==='before-commit') process.exit(71);
        if (mode==='stale-generation') { frame.g='999999'; return originalWrite(packet(frame),...args); }
        if (mode==='stale-request') { frame.r='1'; return originalWrite(packet(frame),...args); }
        if (mode==='partial') { originalWrite(bytes.subarray(0,11),()=>process.exit(74)); return true; }
        if (mode==='invalid-batch') { body.entries.push(['bad']); frame.b=Buffer.from(JSON.stringify(body)).toString('base64'); return originalWrite(packet(frame),...args); }
      }
      if (mode==='after-crypto' && body.kind==='result') process.exit(72);
      if (mode==='lost-stop-reply' && body.kind==='stopped') process.exit(75);
    }
  }
  return originalWrite(bytes,...args);
};
const iterator=process.stdin[Symbol.asyncIterator].bind(process.stdin);
process.stdin[Symbol.asyncIterator]=async function*() {
  let bytes=Buffer.alloc(0);
  for await(const chunk of {[Symbol.asyncIterator]:iterator}) {
    bytes=Buffer.concat([bytes,chunk]);
    while (bytes.length>=4 && bytes.length>=bytes.readUInt32BE()+4) {
      const count=bytes.readUInt32BE()+4, value=bytes.subarray(0,count); bytes=bytes.subarray(count);
      const frame=JSON.parse(value.subarray(4));
      if(retirement && frame.p===0 && frame.e) {
        const body=JSON.parse(Buffer.from(frame.b,'base64'));
        if(['init','call','stop'].includes(body.kind))trace({event:'request',kind:body.kind,request:frame.r,operationId:body.operationId});
      }
      if (frame.r==='2' && frame.p===0 && frame.e) {
        const body=JSON.parse(Buffer.from(frame.b,'base64'));
        if (body.kind==='committed') {
          if (mode==='commit-ack-loss') process.exit(73);
          if (mode==='hold-commit-ack' || mode==='retire-commit-gate') {
            writeFileSync(gate+'.observed','committed');
            while (!existsSync(gate+'.release')) await delay(10);
          }
        }
      }
      yield value;
    }
  }
  if(bytes.length)yield bytes;
};
