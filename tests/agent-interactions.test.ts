import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeInteractions } from '../services/agent-manager/src/interactions.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Agent } from '../packages/contracts/src/generated.js';

test('interactive dispatch joins concurrent duplicates without retaining an independent durable operation',async()=>{
  let resolve!:(reply:Agent.Reply)=>void,calls=0;
  const cache=new NativeInteractions({request:async()=>{calls++;return new Promise<Agent.Reply>(done=>{resolve=done;});}},'epoch');
  const original=cache.run('alice','message','turn/start',{threadId:'main'});
  const duplicate=cache.run('alice','message','turn/start',{threadId:'main'});
  assert.equal(calls,1);assert.throws(()=>cache.run('alice','message','turn/start',{threadId:'other'}),{code:'mutation_conflict'});
  assert.throws(()=>cache.lookup('bob','message'),{code:'interaction_expired'});
  resolve({result:{turn:{id:'turn'}}});assert.deepEqual(await duplicate,await original);
  assert.deepEqual(await cache.lookup('alice','message'),{operationId:'message',epoch:'epoch',reply:{result:{turn:{id:'turn'}}}});
});

test('interactive capacity rejection can retry, while uncertain native effects are never dispatched twice',async()=>{
  let calls=0;
  const cache=new NativeInteractions({request:async()=>{if(++calls===1)throw new IvyError('native_capacity','Busy');throw new IvyError('outcome_unknown','Native connection lost','unknown');}},'epoch');
  await assert.rejects(cache.run('caller','id','turn/start',{}),{code:'native_capacity'});
  await assert.rejects(cache.run('caller','id','turn/start',{}),{code:'outcome_unknown'});
  await assert.rejects(cache.run('caller','id','turn/start',{}),{code:'outcome_unknown'});assert.equal(calls,2);
  const small=new NativeInteractions({request:async()=>({result:{}})},'epoch',{count:2,bytes:1024,ttlMs:60_000});
  await small.run('caller','one','method',{});await small.run('caller','two','method',{});await small.run('caller','three','method',{});
  assert.throws(()=>small.lookup('caller','one'),{code:'interaction_expired'});
});
