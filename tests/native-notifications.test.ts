import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeNotifications, nativeBrowserNotice, inputBrowserNotice } from '../services/agent-manager/src/notifications.js';
const owner={serviceNodeId:'agent',nativeVersion:'0.154.0'};
test('browser notices include completed turns and actionable inputs with exact deep links', () => {
  const event = { method: 'turn/completed', params: { threadId: 'task & one', turn: { id: 'turn-one', status: 'completed' } } };
  const notice = nativeBrowserNotice(owner.serviceNodeId, event)!;
  assert.equal(new URLSearchParams(notice.target.fragment.split('?')[1]).get('id'), 'task & one');
  assert.equal(nativeBrowserNotice(owner.serviceNodeId, { ...event, method: 'item/agentMessage/delta' }), null);
  assert.equal(nativeBrowserNotice(owner.serviceNodeId, { ...event, params: { ...event.params, turn: { id: 'turn-one', status: 'interrupted' } } }), null);
  assert.match(nativeBrowserNotice(owner.serviceNodeId, { ...event, params: { ...event.params, turn: { id: 'turn-one', status: 'failed' } } })!.title, /needs attention/);
  const input = { identity: { serviceNodeId: 'agent', epoch: 'epoch', requestId: 1 }, threadId: 'task & one', state: 'pending' as const, method: 'item/tool/requestUserInput' };
  assert.ok(inputBrowserNotice(input));
  assert.equal(inputBrowserNotice({ ...input, method: 'currentTime/read' }), null);
  assert.equal(inputBrowserNotice({ ...input, state: 'answered' }), null);
});
test('native events require no storage and a restarted event buffer explicitly reports lost history',()=>{
  const log=new NativeNotifications(owner,{start:0,end:1000},1048576);
  const first=log.observe('epoch','turn/started',{threadId:'main'});
  const second=log.observe('epoch','item/completed',{threadId:'main',text:'answer'});assert.equal(second.sequence,first.sequence+1);
  const page=log.page({afterSequence:0},'epoch');assert.deepEqual(page.items,[first,second]);assert.equal(page.gap,false);
  page.items[0]!.method='changed';assert.equal(log.page({afterSequence:0},'epoch').items[0]!.method,'turn/started');
  assert.equal(log.page({afterSequence:second.sequence},'epoch').gap,false);
  const reopened=new NativeNotifications(owner,{start:1000,end:2000},1048576),next=reopened.observe('next','turn/started',{threadId:'main'});
  const recovery=reopened.page({afterSequence:second.sequence},'next');assert.equal(recovery.gap,true);assert.deepEqual(recovery.items,[next]);
});
test('bounded event cache reports eviction and paginates without skipping retained events',()=>{
  const log=new NativeNotifications(owner,{start:0,end:1000},1048576);
  log.observe('epoch','item/agentMessage/delta',{delta:'a'.repeat(600000)});
  const retained=log.observe('epoch','item/agentMessage/delta',{delta:'b'.repeat(600000)}),final=log.observe('epoch','turn/completed',{threadId:'main'});
  const page=log.page({afterSequence:0,limit:1},'epoch');assert.equal(page.gap,true);assert.equal(page.hasMore,true);assert.deepEqual(page.items,[retained]);
  const next=log.page({afterSequence:page.throughSequence},'epoch');assert.deepEqual(next.items,[final]);assert.equal(next.hasMore,false);
});
