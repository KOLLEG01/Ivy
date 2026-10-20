// Fault injection for the isolated auth child, not part of the runtime adapter.
import {existsSync,writeFileSync} from 'node:fs';
import {setTimeout as delay} from 'node:timers/promises';
const mode=process.env.IVY_CHAT_AUTH_FAULT, gate=process.env.IVY_CHAT_AUTH_GATE;
const originalWrite=process.stdout.write.bind(process.stdout);
function packet(frame) {
  const bytes=Buffer.from(JSON.stringify(frame)), header=Buffer.alloc(4);
  header.writeUInt32BE(bytes.length); return Buffer.concat([header,bytes]);
}
process.stdout.write=function(bytes,...args) {
  if (Buffer.isBuffer(bytes) && bytes.length>=4 && bytes.readUInt32BE()===bytes.length-4) {
    const frame=JSON.parse(bytes.subarray(4));
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
      if (frame.r==='2' && frame.p===0 && frame.e) {
        const body=JSON.parse(Buffer.from(frame.b,'base64'));
        if (body.kind==='committed') {
          if (mode==='commit-ack-loss') process.exit(73);
          if (mode==='hold-commit-ack') {
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
