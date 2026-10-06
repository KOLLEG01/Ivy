import test from 'node:test';
import assert from 'node:assert/strict';
import { chatNativeFixture } from './fixtures/chat-native.js';
import { ChatDelivery } from '../services/chat-bridge/src/delivery.js';
import { validateChat } from '../packages/contracts/src/validation.js';
import { chatContracts, version as chatContractVersion } from '../services/chat-bridge/src/schema.js';
import { chatTurnOptions, resolveChatPlan } from '../services/chat-bridge/src/native-plan.js';
import { ChatStore } from '../services/chat-bridge/src/store.js';
import { WhatsAppNative } from '../services/chat-bridge/src/whatsapp/native.js';
import { WhatsAppJournal } from '../services/chat-bridge/src/whatsapp/journal.js';
import { ChatBridge } from '../services/chat-bridge/src/bridge.js';
import { mkdtempSync,rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Chat, Wire } from '../packages/contracts/src/generated.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { mainInstructions, mainThreadOptions, mainTurnContext } from '../services/chat-bridge/src/main-context.js';
import { serviceTools } from '../packages/sdk/src/client.js';
import { WhatsAppRuntime } from '../services/chat-bridge/src/whatsapp/runtime.js';
import { encode } from '../services/chat-bridge/src/whatsapp/auth.js';
import { translator } from '../services/chat-bridge/src/whatsapp/messages.js';

test('a rejected definition change retains the WhatsApp request and media through restart, then dispatches once', async t => {
  const f = await chatNativeFixture(t), created = await f.first.create('alice', f.request('definition-main'));
  if (created.outcome?.action !== 'createMain') throw Error('No binding');
  const root = mkdtempSync(join(tmpdir(), 'wa-definition-'));
  let journal = new WhatsAppJournal(join(root, 'wa.sqlite'));
  t.after(async () => { await journal.close(); rmSync(root, { recursive: true, force: true }); });
  const request: Chat.SendRequest = { action: 'send', operationId: f.operation('definition-input'), messageId: 'definition-input',
    expectedBridge: f.admission.expected('alice'), expectedBinding: created.outcome.binding.object, channel: f.channel,
    payload: { text: 'Transcription of the original voice message', images: [] } };
  journal.accept({ id: request.messageId, sender: 'alice', jid: 'fixture', received: Date.now(),
    message: encode({ message: { audioMessage: { seconds: 5 } } }) });
  journal.saveRequest(request.messageId, request);
  journal.set('direct-input:' + request.messageId, true);
  journal.set('transcript:' + request.messageId, 'Original voice message');
  journal.set('payload:' + request.messageId, request.payload);
  const bridge = new ChatBridge(f.first), native = new WhatsAppNative(bridge, journal);
  const original = f.clients.first.client.request.bind(f.clients.first.client);
  let refused = false;
  f.clients.first.client.request = async (method, params, options) => {
    if (!refused && method === 'tools.call' && 'qualifiedName' in params && params.qualifiedName === 'agent.interact' &&
      (params.arguments as Record<string, Wire.Json>)['method'] === 'turn/start') {
      refused = true;
      throw new IvyError('tool_definition_changed', 'Rejected before dispatch.', 'not_executed');
    }
    return original(method, params, options);
  };
  const run = WhatsAppRuntime.prototype as unknown as { input(this: unknown): Promise<void> };
  const runtime = (selected: WhatsAppNative) => ({ attached: { bridge, native: selected }, journal, text: () => '',
    t: translator(), complete: (row: { id: string }) => journal.finish(row.id) });
  await assert.rejects(run.input.call(runtime(native)), { code: 'tool_definition_changed' });
  assert.deepEqual(JSON.parse(journal.pending()[0]!.request!), request);
  assert.equal(journal.get('transcript:' + request.messageId), 'Original voice message');
  assert.equal(journal.outgoing().length, 0);
  const retained = journal.get<{ call: unknown; attempted: boolean }>('interaction:input:' + request.messageId)!;
  assert.equal(retained.attempted, false);
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
  await journal.close(); journal = new WhatsAppJournal(join(root, 'wa.sqlite'));
  const reopened = new WhatsAppNative(bridge, journal);
  await run.input.call(runtime(reopened));
  await run.input.call(runtime(reopened));
  const turns = f.frames.filter(frame => frame['method'] === 'turn/start');
  assert.equal(turns.length, 1);
  assert.deepEqual((turns[0]!['params'] as Record<string, Wire.Json>)['input'], [{ type: 'text', text: request.payload.text }]);
  assert.equal(journal.pending().length, 0);
});

test('an uncertain definition-change response reconciles the original turn after restart without resending', async t => {
  const f = await chatNativeFixture(t), created = await f.first.create('alice', f.request('uncertain-definition-main'));
  if (created.outcome?.action !== 'createMain') throw Error('No binding');
  const journal = new WhatsAppJournal(':memory:'); t.after(() => journal.close());
  const bridge = new ChatBridge(f.first), native = new WhatsAppNative(bridge, journal);
  const request: Chat.SendRequest = { action: 'send', operationId: f.operation('uncertain-definition-input'), messageId: 'uncertain-definition-input',
    expectedBridge: f.admission.expected('alice'), expectedBinding: created.outcome.binding.object, channel: f.channel,
    payload: { text: 'Original input', images: [] } };
  const original = f.clients.first.client.request.bind(f.clients.first.client);
  let lost = false;
  f.clients.first.client.request = async (method, params, options) => {
    const result = await original(method, params, options);
    if (!lost && method === 'tools.call' && 'qualifiedName' in params && params.qualifiedName === 'agent.interact' &&
      (params.arguments as Record<string, Wire.Json>)['method'] === 'turn/start') {
      lost = true;
      throw new IvyError('tool_definition_changed', 'Uncertain provider response.', 'unknown');
    }
    return result;
  };
  await assert.rejects(native.send('alice', request), { code: 'tool_definition_changed', outcome: 'unknown' });
  const retained = journal.get<{ call: { params: Record<string, Wire.Json> }; attempted: boolean }>('interaction:input:' + request.messageId)!;
  assert.equal(retained.attempted, true);
  f.restart();
  const reopened = new WhatsAppNative(bridge, journal);
  // An expired response without native evidence cannot authorize another dispatch.
  f.reply(frame => frame['method'] === 'thread/turns/list' ? { result: { data: [], nextCursor: null } } : undefined);
  await assert.rejects(reopened.send('alice', request), { code: 'interaction_expired' });
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
  f.reply(frame => {
    if (frame['method'] === 'thread/items/list') return { error: { code: -32601, message: 'No item pages.' } };
    if (frame['method'] !== 'thread/turns/list') return undefined;
    const itemsView = (frame['params'] as Record<string, Wire.Json>)['itemsView']!;
    return { result: { data: [{ id: 'original-turn', status: 'completed', completedAt: 100, error: null, itemsView,
      items: itemsView === 'full' ? [{ id: 'original-user', type: 'userMessage', clientId: retained.call.params['clientUserMessageId']!, content: [] }] : [] }], nextCursor: null } };
  });
  assert.ok(await reopened.send('alice', request));
  assert.ok(await reopened.send('alice', request));
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('Main owns delivery in started and resumed threads while other agent threads retain their tools', () => {
  const template = { developerInstructions: 'Personal settings', config: { 'mcp_servers.ivy.disabled_tools': ['another_tool'], 'features.apps': false } };
  const settings = mainThreadOptions(template);
  assert.deepEqual((settings['config'] as Record<string, unknown>)['mcp_servers.ivy.disabled_tools'], ['another_tool', 'chat_bridge_send']);
  assert.match(String(settings['developerInstructions']), /Personal settings/);
  assert.ok(String(settings['developerInstructions']).includes(mainInstructions));
  assert.deepEqual(template.config['mcp_servers.ivy.disabled_tools'], ['another_tool']);
  const context = mainTurnContext({ additionalContext: { personal: { kind: 'untrusted', value: 'saved' } } }, 'service notification');
  assert.deepEqual((context['additionalContext'] as Record<string, unknown>)['personal'], { kind: 'untrusted', value: 'saved' });
  assert.match(JSON.stringify(context), /Input origin: service notification/);
});

test('direct WhatsApp Main input bypasses Object writes and recovers one native turn with retained preferences',async t=>{
  const f=await chatNativeFixture(t),created=await f.first.create('alice',f.request('direct-main'));
  if(created.outcome?.action!=='createMain')throw Error('No binding');
  const root=mkdtempSync(join(tmpdir(),'wa-direct-')),journal=new WhatsAppJournal(join(root,'wa.sqlite'));
  t.after(async()=>{await journal.close();rmSync(root,{recursive:true,force:true});});
  const native=new WhatsAppNative(new ChatBridge(f.first),journal);
  const bytes=Buffer.alloc(900000);bytes.set([255,216,255]);
  const main=await f.first.queue.main();
  const media=await f.first.store.client.request('objects.write',{mutationId:'direct-image',contractVersion:'1.0.0',references:{},create:{parentId:f.first.store.rootObjectId,ownerObjectId:main.value.conversation.objectId,contractKey:'chat-bridge/image-jpeg',name:'Direct image'},content:{encoding:'base64',value:bytes.toString('base64')}});
  const request:Chat.SendRequest={action:'send',operationId:f.operation('direct-send'),messageId:'direct-message',expectedBridge:f.admission.expected('alice'),expectedBinding:created.outcome.binding.object,channel:f.channel,payload:{text:'Direct input',images:[{object:{objectId:media.object.id,revision:media.revision.revision},contentHash:digest(bytes),byteLength:bytes.length,mediaType:'image/jpeg',label:'Large image'}],turnOptions:{model:'gpt-5.6-luna',effort:'high'}}};
  const original=f.first.store.client.request.bind(f.first.store.client);
  f.first.store.client.request=async(method,args,options)=>{assert.notEqual(method,'objects.write');return original(method,args,options);};
  await native.prepare();native.setBusy(true);
  await assert.rejects(native.send('alice',request),{code:'native_read_pending'});
  assert.equal(f.frames.filter(frame=>frame['method']==='turn/start').length,0);
  native.setBusy(false);
  const methodStart=f.methods.length,frameStart=f.frames.length;
  f.loseReply('turn/start');await assert.rejects(native.send('alice',request),{code:'outcome_unknown'});
  f.unavailable(true);await assert.rejects(native.send('alice',request),{code:'service_not_ready'});f.unavailable(false);
  const reply=await native.send('alice',request);assert.ok(reply);
  assert.equal(f.frames.slice(frameStart).filter(frame=>frame['method']==='thread/read').length,0);
  assert.deepEqual(f.methods.slice(methodStart).filter(method=>['agent.status','agent.operation'].includes(method)),[]);
  assert.ok(f.methods.slice(methodStart).includes('agent.interact'));
  const reopened=new WhatsAppNative(new ChatBridge(f.first),journal);
  assert.deepEqual(await reopened.send('alice',request),reply);
  const turns=f.frames.filter(frame=>frame['method']==='turn/start');assert.equal(turns.length,1);
  const inputs=(turns[0]!.params as {input:Array<Record<string,unknown>>}).input;
  assert.deepEqual(inputs[0],{type:'text',text:'Direct input'});assert.equal(inputs.length,2);
  assert.equal(inputs[1]!['type'],'image');assert.equal(inputs[1]!['url'],'data:image/jpeg;base64,'+bytes.toString('base64'));
  assert.equal((turns[0]!.params as Record<string,unknown>)['model'],'gpt-5.6-luna');
  assert.equal((turns[0]!.params as Record<string,unknown>)['effort'],'high');
  assert.deepEqual((await f.first.find())!.value.queue,[]);
  await assert.rejects(reopened.send('alice',{...request,payload:{...request.payload,text:'Changed'}}),{code:'whatsui_identity_conflict'});
  native.setBusy(false);const access=f.accesses.length;
  assert.ok(await native.send('alice',{...request,messageId:'next-text',operationId:f.operation('next-text'),payload:{text:'Next',images:[]}}));
  assert.ok(!f.accesses.slice(access).some(method=>method.startsWith('objects.')),'Warm text turn needs no Hive object read or write.');
});

test('exact Chat plans are checked once per store generation; changed pins and failed reads never reuse success',async t=>{
  const f=await chatNativeFixture(t),store=f.first.store,plan=f.admission.definition.nativePlan;
  const before=f.accesses.length;
  const [first,second]=await Promise.all([resolveChatPlan(store,plan),resolveChatPlan(store,plan)]);
  assert.equal(f.accesses.length-before,0);
  first.threadStart['cwd']='tampered';assert.notDeepEqual(first,second);
  assert.deepEqual(await resolveChatPlan(store,plan),second);assert.equal(f.accesses.length-before,0);
  await assert.rejects(resolveChatPlan(store,{...plan,turnStart:{...plan.turnStart,catalogHash:'sha256:'+'0'.repeat(64)}}));
  const fresh=new ChatStore({request:async()=>{throw Error('Local plans need no RPC.');}},store.rootObjectId);
  const count=f.accesses.length;assert.deepEqual(await resolveChatPlan(fresh,plan),second);
  assert.equal(f.accesses.length-count,0);
});

test('Main survives Agent registry updates and recovers a lost WhatsApp reply without replaying the message', async t => {
  const f = await chatNativeFixture(t), created = await f.first.create('alice', f.request('registry-update-main'));
  if (created.outcome?.action !== 'createMain') throw Error('No binding');
  const binding = created.outcome.binding, journal = new WhatsAppJournal(':memory:');
  t.after(() => journal.close());
  const native = new WhatsAppNative(new ChatBridge(f.first), journal);
  await native.prepare();
  const readParams = { threadId: binding.data.primary.nativeId, includeTurns: false };
  await native.read('thread/read', readParams);
  await assert.rejects(native.owner.interaction('not-sent'), { code: 'interaction_expired' });
  const registry = f.kernel.registry.getRegistry('native-agent')!;
  for (const tool of registry.namespaces.find(ns => ns.namespace === 'agent')!.tools)
    tool.description += ' Updated service documentation.';
  await f.clients.agent.client.request('registry.sync', registry);
  const tools = serviceTools(f.first.store.client, 'native-agent', [{ namespace: 'agent', interfaceVersion: '1.0.0' }]);
  await assert.rejects(tools.call('agent.read', { nativeVersion: f.admission.definition.nativePlan.nativeVersion, method: 'thread/read', params: readParams }),
    { code: 'tool_definition_changed', outcome: 'not_executed' });
  native.invalidate(true);
  await native.prepare();
  assert.ok(await native.read('thread/read', readParams));
  const request: Chat.SendRequest = { action: 'send', operationId: f.operation('registry-update-input'), messageId: 'registry-update-input',
    expectedBridge: f.admission.expected('alice'), expectedBinding: binding.object, channel: f.channel,
    payload: { text: 'Keep this original message', images: [], turnOptions: { model: 'gpt-5.6-luna', effort: 'high' } } };
  f.loseReply('turn/start');
  await assert.rejects(native.send('alice', request), { code: 'outcome_unknown' });
  const reply = await native.send('alice', request);
  assert.ok(reply);
  assert.deepEqual(await native.send('alice', request), reply);
  const turns = f.frames.filter(frame => frame['method'] === 'turn/start');
  assert.equal(turns.length, 1);
  const params = turns[0]!['params'] as Record<string, Wire.Json>;
  assert.equal(params['threadId'], binding.data.primary.nativeId);
  assert.deepEqual(params['input'], [{ type: 'text', text: request.payload.text }]);
  assert.equal(params['model'], 'gpt-5.6-luna');
  assert.deepEqual((await f.first.find())!.value.binding, binding.object);
});

test('unloaded Main resumes its bound identity using one cached interaction',async t=>{
  const f=await chatNativeFixture(t),created=await f.first.create('alice',f.request('main-resume'));
  assert.equal(created.outcome?.action,'createMain');if(created.outcome?.action!=='createMain')throw Error('No binding');
  const binding=created.outcome.binding;
  const root=mkdtempSync(join(tmpdir(),'wa-resume-')),journal=new WhatsAppJournal(join(root,'wa.sqlite'));
  t.after(async()=>{await journal.close();rmSync(root,{recursive:true,force:true});});
  const native=new WhatsAppNative(new ChatBridge(f.first),journal),originalRead=native.owner.read.bind(native.owner);
  for (const error of [{code:-32600,message:'thread not loaded: unrelated'}, {code:-32603,message:'thread not loaded: '+binding.data.primary.nativeId}]) {
    native.owner.read=async(method,params,epoch)=>({...await originalRead(method,params,epoch,true),reply:{error}});
    await assert.rejects(native.read('thread/read',{threadId:binding.data.primary.nativeId,includeTurns:false}),
      {code:error.code===-32600?'chat_native_mismatch':'native_read_failed'});
    assert.equal(f.frames.filter(frame=>frame['method']==='thread/resume').length,0);
  }
  let unloaded=true;native.owner.read=async(method,params,epoch,allowError)=>{
    assert.equal(allowError,true);const result=await originalRead(method,params,epoch,true);
    if(!unloaded)return result;unloaded=false;return {...result,reply:{error:{code:-32600,message:'thread not loaded: '+binding.data.primary.nativeId}}};
  };
  await assert.rejects(native.read('thread/read',{threadId:binding.data.primary.nativeId,includeTurns:false}),{code:'native_read_pending'});
  for(let i=0;i<2;i++)assert.ok(await native.read('thread/read',{threadId:binding.data.primary.nativeId,includeTurns:false}));
  assert.equal(f.frames.filter(frame=>frame['method']==='thread/resume').length,1);
  const saved=journal.db.prepare("SELECT value FROM kv WHERE key LIKE 'interaction:%'").all().map(row=>JSON.parse(String(row['value'])));
  assert.equal(saved.length,0,'Attaching native notifications needs no durable operation.');
});

test('an internal native error mentioning an active turn cannot authorize another WhatsApp dispatch', async t => {
  const f = await chatNativeFixture(t), created = await f.first.create('alice', f.request('internal-error'));
  if (created.outcome?.action !== 'createMain') throw Error('No binding');
  const journal = new WhatsAppJournal(':memory:'); t.after(() => journal.close());
  const native = new WhatsAppNative(new ChatBridge(f.first), journal);
  f.reply(frame => frame['method'] === 'turn/start' ? {error:{code:-32603,message:'Could not persist turn already active'}} : undefined);
  const request: Chat.SendRequest = {action:'send',operationId:f.operation('uncertain-turn'),messageId:'uncertain-turn',
    expectedBridge:f.admission.expected('alice'),expectedBinding:created.outcome.binding.object,channel:f.channel,payload:{text:'Original',images:[]}};
  for (let i = 0; i < 2; i++) await assert.rejects(native.send('alice', request), {code:'whatsapp_native_command_failed'});
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 1);
});

test('preferences override native collaboration model and reasoning without changing mode or instructions',()=>{
  const template={collaborationMode:{mode:'default',settings:{model:'original',reasoning_effort:'xhigh',developer_instructions:'retained'}},cwd:'C:/main'};
  assert.deepEqual(chatTurnOptions(template,{model:'selected',effort:'low',serviceTier:'fast'}),{...template,model:'selected',effort:'low',serviceTier:'fast',collaborationMode:{mode:'default',settings:{model:'selected',reasoning_effort:'low',developer_instructions:'retained'}}});
  assert.deepEqual(chatTurnOptions(template,{model:null,effort:null}).collaborationMode,template.collaborationMode);
});

test('a missing native Main recovers the original unsent WhatsApp input with one replacement and one turn', async t => {
  const f = await chatNativeFixture(t), created = await f.first.create('alice', f.request('recover-main'));
  if (created.outcome?.action !== 'createMain') throw Error('No binding');
  const old = created.outcome.binding, root = mkdtempSync(join(tmpdir(), 'wa-heal-'));
  let journal = new WhatsAppJournal(join(root, 'wa.sqlite'));
  t.after(async () => { await journal.close(); rmSync(root, {recursive:true,force:true}); });
  const request: Chat.SendRequest = {action:'send',operationId:f.operation('heal-input'),messageId:'heal-input',
    expectedBridge:f.admission.expected('alice'),expectedBinding:old.object,channel:f.channel,
    payload:{text:'Original input',images:[],turnOptions:{model:'gpt-5.6-luna',effort:'high'}}};
  journal.accept({id:request.messageId,sender:'alice',jid:'fixture',message:'Original input',received:Date.now()});
  journal.saveRequest(request.messageId,request);
  f.reply(frame => {
    const params = frame['params'] as {threadId?:string};
    if (params.threadId !== old.data.primary.nativeId) return undefined;
    if (frame['method'] === 'thread/resume') return {error:{code:-32600,message:`no rollout found for thread id ${params.threadId}`}};
    if (frame['method'] === 'thread/read') return {error:{code:-32600,message:`thread not loaded: ${params.threadId}`}};
    return undefined;
  });
  const native = new WhatsAppNative(new ChatBridge(f.first),journal);
  const create = f.first.create.bind(f.first);
  let interrupted = false;
  f.first.create = async (...args) => {
    const result = await create(...args);
    if (!interrupted) { interrupted=true; throw new IvyError('outcome_unknown','Creation reply lost','unknown'); }
    return result;
  };
  await assert.rejects(native.send('alice',request),{code:'outcome_unknown'});
  await journal.close(); journal = new WhatsAppJournal(join(root,'wa.sqlite'));
  const reopened = new WhatsAppNative(new ChatBridge(f.first),journal);
  assert.equal(await reopened.send('alice',request),null);
  const rebound = JSON.parse(journal.pending()[0]!.request!) as Chat.SendRequest;
  assert.notEqual(rebound.expectedBinding?.objectId,old.object.objectId);
  assert.deepEqual({...rebound,expectedBinding:old.object},request);
  assert.ok(await reopened.send('alice',rebound));
  assert.ok(await reopened.send('alice',rebound));
  assert.equal(f.frames.filter(frame=>frame['method']==='thread/start').length,2);
  const turns = f.frames.filter(frame=>frame['method']==='turn/start');
  assert.equal(turns.length,1);
  assert.deepEqual((turns[0]!.params as {input:unknown}).input,[{type:'text',text:'Original input'}]);
});

test('a resume response cannot overwrite a newer native busy notification', async t => {
  const f = await chatNativeFixture(t), created = await f.first.create('alice', f.request('resume-race'));
  if (created.outcome?.action !== 'createMain') throw Error('No binding');
  const journal = new WhatsAppJournal(':memory:'); t.after(() => journal.close());
  const native = new WhatsAppNative(new ChatBridge(f.first), journal);
  const interact = native.owner.interact.bind(native.owner);
  native.owner.interact = async (...args) => {
    const response = await interact(...args);
    native.setBusy(true);
    return response;
  };
  await native.prepare();
  await assert.rejects(native.send('alice', {action:'send',operationId:f.operation('busy-send'),messageId:'busy-send',
    expectedBridge:f.admission.expected('alice'),expectedBinding:created.outcome.binding.object,channel:f.channel,
    payload:{text:'Wait for the current turn',images:[]}}), {code:'native_read_pending'});
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
});

test('a newly created unmaterialized Main has an empty listener baseline without native resume or another start',async t=>{
  const f=await chatNativeFixture(t),created=await f.first.create('alice',f.request('empty-main'));
  if(created.outcome?.action!=='createMain')throw Error('No binding');
  const id=created.outcome.binding.data.primary.nativeId,root=mkdtempSync(join(tmpdir(),'wa-empty-')),journal=new WhatsAppJournal(join(root,'wa.sqlite'));
  t.after(async()=>{await journal.close();rmSync(root,{recursive:true,force:true});});
  const native=new WhatsAppNative(new ChatBridge(f.first),journal),originalRead=native.owner.read.bind(native.owner);
  native.owner.interact=async()=>({reply:{error:{code:-32600,message:`no rollout found for thread id ${id}`}}} as never);
  await native.prepare();
  native.invalidate(true);
  native.owner.read=async(...args)=>({...await originalRead(...args),
    reply:{error:{code:-32600,message:`thread not loaded: ${id}`}}});
  await assert.rejects(native.prepare(),{code:'chat_main_unavailable'});
  native.owner.read=originalRead;
  native.owner.read=async(method,params,epoch)=>({...await originalRead('thread/read',{threadId:id,includeTurns:false},epoch,true),
    reply:{error:{code:-32600,message:`thread ${id} is not materialized yet; thread/turns/list is unavailable before first user message`}}});
  const starts=()=>f.frames.filter(frame=>['thread/start','thread/resume'].includes(String(frame['method']))).length,before=starts();
  assert.deepEqual(await native.read('thread/turns/list',{threadId:id,limit:1,itemsView:'full',cursor:null,sortDirection:'desc'}),{data:[],nextCursor:null});
  await assert.rejects(native.read('thread/read',{threadId:id,includeTurns:false}),{code:'native_read_failed'});
  assert.equal(starts(),before);
});

test('WhatsApp channel and turn preferences retain the exact admitted native request through queue delivery',async t=>{
  const f=await chatNativeFixture(t,'0.154.0',definition=>{definition.channels[0]!.channel.adapter='whatsapp';});
  const created=await f.first.create('alice',f.request('main'));assert.equal(created.outcome?.action,'createMain');if(created.outcome?.action!=='createMain')throw Error('No binding');
  const request={action:'send' as const,operationId:f.operation('wa-original'),messageId:'wa-original',channel:f.channel,expectedBridge:f.admission.expected('alice'),expectedBinding:created.outcome.binding.object,
    payload:{text:'Native input',images:[],turnOptions:{model:'chosen-model',effort:'high' as const,serviceTier:'fast' as const}}};
  const result=await f.first.queue.send('alice',request);assert.equal(result.phase,'succeeded');
  const delivery=new ChatDelivery(f.first);for(let i=0;i<5;i++)await delivery.step();
  const frames=f.frames.filter(frame=>frame['method']==='turn/start');assert.equal(frames.length,1);
  const params=frames[0]!['params'] as Record<string,unknown>;assert.equal(params['model'],'chosen-model');assert.equal(params['effort'],'high');assert.equal(params['serviceTier'],'fast');
  await delivery.step();assert.equal(f.frames.filter(frame=>frame['method']==='turn/start').length,1);
});
test('transport preference payload cannot override permissions, thread, input or working directory',()=>{
  for(const key of ['cwd','permissions','threadId','input','approvalPolicy','developerInstructions'])assert.throws(()=>validateChat('Payload',{text:'x',images:[],turnOptions:{[key]:'injected'}}));
});

test('WhatsApp retains an unsent turn during expired native authentication and resumes it once after sign-in', async t => {
  const f = await chatNativeFixture(t), created = await f.first.create('alice', f.request('auth-main'));
  if (created.outcome?.action !== 'createMain') throw Error('No binding');
  const journal = new WhatsAppJournal(':memory:'); t.after(() => journal.close());
  const native = new WhatsAppNative(new ChatBridge(f.first), journal);
  let signedIn = false, now = Date.now(); t.mock.method(Date, 'now', () => now);
  f.reply(frame => frame['method'] === 'account/read' ? { result: { requiresOpenaiAuth: true,
    account: signedIn ? { type: 'chatgpt', email: 'fixture@example.test', planType: 'pro' } : null } } : undefined);
  const request: Chat.SendRequest = { action: 'send', operationId: f.operation('auth-input'), messageId: 'auth-input',
    expectedBridge: f.admission.expected('alice'), expectedBinding: created.outcome.binding.object, channel: f.channel,
    payload: { text: 'Original message', images: [], turnOptions: { model: 'chosen-model', effort: 'high' } } };
  await assert.rejects(native.send('alice', request), { code: 'chat_auth_required', outcome: 'not_executed' });
  const retained = journal.get<{call: unknown; attempted: boolean}>('interaction:input:auth-input')!;
  assert.equal(retained.attempted, false);
  await assert.rejects(native.send('alice', request), { code: 'chat_auth_required' });
  assert.equal(f.frames.filter(frame => frame['method'] === 'account/read').length, 1, 'Blocked inputs do not flood the authentication service.');
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
  signedIn = true; now += 30_001;
  const result = await native.send('alice', request); assert.ok(result);
  assert.deepEqual(await new WhatsAppNative(new ChatBridge(f.first), journal).send('alice', request), result);
  assert.deepEqual(journal.get<{call: unknown}>('interaction:input:auth-input')!.call, retained.call);
  const turns = f.frames.filter(frame => frame['method'] === 'turn/start'); assert.equal(turns.length, 1);
  assert.deepEqual((turns[0]!['params'] as Record<string, unknown>)['input'], [{ type: 'text', text: 'Original message' }]);
});

test('WhatsApp recovery reads one metadata turn and complete item pages, reusing only unchanged terminal contents', async t => {
  const f = await chatNativeFixture(t, '0.158.0'), created = await f.first.create('alice', f.request('paged-history'));
  if (created.outcome?.action !== 'createMain') throw Error('No binding');
  const journal = new WhatsAppJournal(':memory:'); t.after(() => journal.close());
  const native = new WhatsAppNative(new ChatBridge(f.first), journal), threadId = created.outcome.binding.data.primary.nativeId;
  const items: Array<Record<string, Wire.Json>> = [{ id: 'user', type: 'userMessage', clientId: 'original-client-id', content: [] },
    { id: 'tool', type: 'mcpToolCall', result: { text: 'Large tool payload '.repeat(100000) } },
    ...Array.from({ length: 35 }, (_, index) => ({ id: 'answer-' + index, type: 'agentMessage', text: 'Complete answer ' + index }))];
  let completedAt = 100, supported = true;
  f.reply(frame => {
    const params = frame['params'] as Record<string, Wire.Json>;
    if (frame['method'] === 'thread/turns/list') {
      assert.equal(params['limit'], 1);
      return { result: { data: [{ id: 'turn-one', status: 'completed', completedAt, error: null, itemsView: params['itemsView']!, items: params['itemsView'] === 'full' ? items : [] }], nextCursor: null } };
    }
    if (frame['method'] !== 'thread/items/list') return undefined;
    if (!supported) return { error: { code: -32601, message: 'Original owner does not support item pages.' } };
    assert.equal(params['limit'], 16); assert.equal(params['turnId'], 'turn-one');
    const offset = Number(params['cursor'] ?? 0);
    return { result: { data: items.slice(offset, offset + 16).map(item => ({ turnId: 'turn-one', item })), nextCursor: offset + 16 < items.length ? String(offset + 16) : null } };
  });
  const page = await native.turnPage(threadId, null), turn = (page['data'] as Array<Record<string, unknown>>)[0]!;
  assert.equal((turn['items'] as Array<Record<string, unknown>>).length, items.length);
  assert.deepEqual((turn['items'] as Array<Record<string, unknown>>).at(-1), items.at(-1));
  assert.equal((turn['items'] as Array<Record<string, unknown>>)[1]!['result'], undefined);
  assert.equal(f.frames.filter(frame => frame['method'] === 'thread/items/list').length, 3);
  await native.turnPage(threadId, null); assert.equal(f.frames.filter(frame => frame['method'] === 'thread/items/list').length, 3);
  f.restart(); await native.turnPage(threadId, null); assert.equal(f.frames.filter(frame => frame['method'] === 'thread/items/list').length, 6);
  supported = false; completedAt++; await native.turnPage(threadId, null);
  assert.equal(f.frames.filter(frame => frame['method'] === 'thread/turns/list' && (frame['params'] as Record<string, unknown>)['itemsView'] === 'full').length, 1);
  native.owner.interact = async (_call, guard) => { if (guard) await guard(); throw new IvyError('interaction_expired', 'Original response expired.'); };
  const recovered = await native.command('expired-original', 'turn/start', async () => ({ threadId, clientUserMessageId: 'original-client-id', input: [{ type: 'text', text: 'Original' }] }));
  assert.equal((recovered as { turn: { id: string } }).turn.id, 'turn-one');
  assert.equal(f.frames.filter(frame => frame['method'] === 'turn/start').length, 0);
});
test('new Chat registry publishes only current final conversation records and owned media',()=>{
  const contracts=chatContracts(),json=contracts.filter(contract=>contract.mediaType==='application/json');
  assert.deepEqual(json.map(contract=>contract.key).sort(),['chat-bridge/conversation','chat-bridge/input','chat-bridge/reply','chat-bridge/result']);
  assert.ok(json.every(contract=>contract.version===chatContractVersion));
});
