import test from 'node:test';
import assert from 'node:assert/strict';
import { ChatStore } from '../services/chat-bridge/src/store.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Chat, Operation } from '../packages/contracts/src/generated.js';
import type { RpcClient } from '../packages/sdk/src/client.js';

test('ChatStore keeps one current Main state through more than ten thousand updates',async t=>{
  const store=new ChatStore({} as RpcClient,'root');t.after(()=>store.close());
  let value={sequence:0} as unknown as Chat.Main;
  let pin=await store.write('chat-bridge/main',value,'create',{create:{parentId:'root',name:'main'}});
  for(let sequence=1;sequence<=10_050;sequence++){
    value={sequence} as unknown as Chat.Main;
    pin=await store.write('chat-bridge/main',value,'update-'+sequence,{objectId:pin.objectId,expectedRevision:pin.revision});
  }
  assert.equal(pin.revision,10_051);assert.deepEqual((await store.read('chat-bridge/main',pin.objectId)).value,value);
  assert.deepEqual((await store.read('chat-bridge/main',{objectId:pin.objectId,revision:1})).pin,pin);
});

test('ChatStore removes terminal Input child workflows after their first deadline',async t=>{
  const epoch='chat-store-test',inputId='input';
  const client={async request(method:string,params:Record<string,unknown>){
    if(method==='system.status')return{runtimeEpoch:epoch};
    if(method==='objects.write')return{object:{id:inputId},revision:{revision:2}} as Operation.ObjectWriteResult;
    throw Error('Unexpected '+method);
  }} as RpcClient;
  const store=new ChatStore(client,'root');t.after(()=>store.close());
  const child=await store.write('chat-bridge/collection',{} as Chat.Collection,'child',{create:{parentId:inputId,name:'collection'}});
  const input={payload:{images:[]},state:'completed',finishedAt:'2000-01-01T00:00:00.000Z'} as unknown as Chat.Input;
  await store.write('chat-bridge/input',input,'terminal',{objectId:inputId,expectedRevision:1});
  await assert.rejects(store.read('chat-bridge/collection',child,inputId),error=>error instanceof IvyError&&error.code==='chat_scope_mismatch');
});

test('ChatStore expires sibling offers and acknowledgements with their original operation', async t => {
  const store = new ChatStore({} as RpcClient, 'root'); t.after(() => store.close());
  const now = Date.now();
  const value = {phase:'succeeded', updatedAt:new Date(now).toISOString()} as Chat.Operation;
  const expired = await store.write('chat-bridge/operation', value, 'operation', {create:{parentId:'root',name:'expired'}});
  const pending = await store.write('chat-bridge/operation', {...value,phase:'accepted'}, 'pending', {create:{parentId:'root',name:'pending'}});
  for (const operation of [expired, pending]) {
    await store.write('chat-bridge/offer', {} as Chat.Offer, 'offer', {create:{parentId:'root',name:'Chat offer '+operation.objectId}});
    await store.write('chat-bridge/acknowledgement', {} as Chat.Acknowledgement, 'ack', {create:{parentId:'root',name:'Chat acknowledgement '+operation.objectId}});
  }
  const maintenance = store as unknown as {expireOperations(now:number,force:boolean):void};
  maintenance.expireOperations(now + 86_400_001, true);
  for (const [key, prefix] of [['chat-bridge/offer','Chat offer '],['chat-bridge/acknowledgement','Chat acknowledgement ']] as const) {
    assert.equal(await store.named(key, prefix + expired.objectId), null);
    assert.ok(await store.named(key, prefix + pending.objectId));
  }
  assert.equal(await store.named('chat-bridge/operation','expired'),null);
});

test('ChatStore retains completed admission and cancellation receipts until their input leaves the queue', async t => {
  const store = new ChatStore({} as RpcClient, 'root'); t.after(() => store.close());
  const now = Date.now();
  const main = await store.write('chat-bridge/main', {queue:[{inputId:'waiting',identity:{senderPrincipalId:'producer',messageId:'notice'}}]} as Chat.Main, 'main', {create:{parentId:'root',name:'main'}});
  for (const action of ['send','cancel'] as const) {
    await store.write('chat-bridge/operation', {phase:'succeeded',updatedAt:new Date(now).toISOString(),
      outcome:{action,input:{object:{objectId:'waiting',revision:1}}}} as Chat.Operation, action, {create:{parentId:'root',name:action}});
  }
  for (const phase of ['succeeded', 'failed'] as const) {
    await store.write('chat-bridge/operation', {phase,updatedAt:new Date(now).toISOString(),callerPrincipalId:'producer',operationId:'notice',request:{action:'notify'}} as Chat.Operation,
      'notice-'+phase, {create:{parentId:'root',name:'notice-'+phase}});
  }
  const maintenance = store as unknown as {expireOperations(now:number,force:boolean):void};
  maintenance.expireOperations(now + 86_400_001, true);
  for (const action of ['send','cancel','notice-succeeded','notice-failed']) assert.ok(await store.named('chat-bridge/operation', action));
  await store.write('chat-bridge/main', {queue:[]} as unknown as Chat.Main, 'drained', {objectId:main.objectId,expectedRevision:main.revision});
  maintenance.expireOperations(now + 86_400_002, true);
  for (const action of ['send','cancel','notice-succeeded','notice-failed']) assert.equal(await store.named('chat-bridge/operation', action), null);
});
