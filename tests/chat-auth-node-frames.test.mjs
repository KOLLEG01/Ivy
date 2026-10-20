import test from 'node:test';
import assert from 'node:assert/strict';
import {PassThrough} from 'node:stream';
import {authFrames} from '../services/chat-bridge/rust/node/auth-frames.mjs';

const packet=(body,changes={})=>{
  const frame=Buffer.from(JSON.stringify({v:1,g:'17',r:'1',m:'1',p:0,e:true,b:Buffer.from(JSON.stringify(body)).toString('base64'),...changes}));
  const header=Buffer.alloc(4);header.writeUInt32BE(frame.length);return Buffer.concat([header,frame]);
};

test('auth frames preserve large values and lone surrogates with finite pipe backpressure',async()=>{
  const input=new PassThrough({highWaterMark:1024}),unused=new PassThrough();
  const sender=authFrames(unused,input,'17'),receiver=authFrames(input,unused,'17');
  const value={encoded:'a'.repeat(300_000)+'\ud800\0€',keys:['\udfff','unknown-category'],buffer:{type:'Buffer',data:'AAH/'}};
  const writes=[];const original=input.write.bind(input);
  input.write=(bytes,...args)=>{writes.push(bytes.length);return original(bytes,...args);};
  let completed=false;const sending=sender.send('1',value).then(()=>{completed=true;});
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(completed,false,'An unread pipe must apply backpressure');
  assert(input.readableLength<=65536,'Only a bounded frame may wait in the pipe');
  const messages=receiver.messages(),message=await messages.next();
  assert.deepEqual(message.value,{request:'1',value});await sending;
  assert(writes.length>1);assert(Math.max(...writes)<=65536);
  input.end();assert.equal((await messages.next()).done,true);unused.destroy();
});

test('auth framing rejects stale generations, message replay, order and noncanonical chunks',async()=>{
  for(const changes of [{g:'16'},{m:'2'},{p:1},{b:'bm90LWpzb24='},{b:'e30=\n'}]) {
    const input=new PassThrough(),output=new PassThrough(),reader=authFrames(input,output,'17').messages();
    input.end(packet({kind:'set',entries:[['session','synthetic','{}']]},changes));
    await assert.rejects(reader.next());input.destroy();output.destroy();
  }
});

test('partial frames and logical messages never yield a callback',async()=>{
  for(const bytes of [packet({kind:'set'}).subarray(0,11),packet({kind:'set'},{e:false})]) {
    const input=new PassThrough(),output=new PassThrough(),reader=authFrames(input,output,'17').messages();
    input.end(bytes);await assert.rejects(reader.next());input.destroy();output.destroy();
  }
});
