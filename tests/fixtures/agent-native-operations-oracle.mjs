import {NativeOperations} from '../../dist/services/agent-manager/src/operations.js';
import {NativeJournal} from '../../dist/services/agent-manager/src/journal.js';
import {digest,hashJson} from '../../dist/packages/contracts/src/canonical.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';

const owner={hostId:'fixture-host',serviceNodeId:'agent',nativeVersion:'test',nativeExecutableHash:digest('native')};
const limits={maxOperations:100,maxJournalBytes:128*1024*1024,maxPendingInputs:16,maxNotificationBytes:1048576};
const identity=(operationId,callerPrincipalId='caller')=>({callerPrincipalId,operationId});
const c=(action,fields={})=>({action,...fields});
const start=(handle,id=handle,extra={})=>c('start',{handle,identity:identity(id),owner:'a',method:'thread/start',params:{},behavior:'complete',...extra});
const wait=handle=>c('wait',{handle}),read=(id,extra={})=>c('read',{owner:'a',identity:identity(id),...extra});
const snapshot=c('snapshot'),clear=c('clear',{owner:'a'}),failure=(code='service_unavailable',afterCommit=false)=>c('failure',{code,afterCommit});
const addOwner=c('owner',{name:'b'}),gate=name=>c('entered',{gate:name}),release=name=>c('release',{gate:name});
const cases=[];
function add(name,commands,extra={}){cases.push({name,commands,...extra});}
add('successful-replay-and-original-argument-conflict',[
  start('first'),wait('first'),clear,start('replay','first',{owner:'b'}),wait('replay'),
  start('different','first',{owner:'b',params:{different:true}}),wait('different'),snapshot,
],{owners:['a','b']});
add('canonical-arguments-and-caller-isolation',[
  start('one','😀',{identity:identity('😀','\ud800'),method:'thread/😀',params:{'2':2,'1':1,'\ud800':'\udc00'}}),wait('one'),
  start('same','😀',{identity:identity('😀','\ud800'),method:'thread/😀',params:{'\ud800':'\udc00','1':1,'2':2}}),wait('same'),
  start('other','😀',{identity:identity('😀','other')}),wait('other'),snapshot,
]);
add('prevented-never-dispatches',[start('prevented','prevented',{prevent:true}),wait('prevented'),read('prevented'),clear,
  start('again','prevented'),wait('again'),snapshot]);
for(const code of ['mutation_conflict','revision_conflict','path_conflict','name_conflict','already_exists','service_unavailable'])
  add('failed-claim-'+code,[failure(code),start('failed'),wait('failed'),snapshot,start('retry','failed'),wait('retry'),snapshot]);
for(const code of ['mutation_conflict','revision_conflict','path_conflict','name_conflict','already_exists'])
  add('competing-claim-'+code,[failure(code,true),start('one'),wait('one'),snapshot]);
add('uncertain-claim-is-never-sent',[failure('service_unavailable',true),start('uncertain'),wait('uncertain'),snapshot,
  start('retry','uncertain'),wait('retry'),snapshot]);
add('terminal-write-failure-recovers-on-read',[start('one','one',{behavior:'terminalWriteFailure'}),wait('one'),snapshot,read('one'),snapshot,
  start('next'),wait('next'),snapshot]);
add('stale-final-pin-recovers-from-live-result',[start('one','one',{gate:'first'}),gate('first'),c('patch',{id:'one',patch:{},bump:true}),release('first'),wait('one'),snapshot,read('one'),snapshot]);
for(const behavior of ['reject','lost','terminalThrow','unknownThrow','forgetThrow','clearThrow','returnLive'])
  add('action-'+behavior,[start('one','one',{behavior}),wait('one'),read('one'),snapshot,clear,read('one'),start('retry','one',{owner:'b'}),wait('retry'),snapshot],{owners:['a','b']});
add('duplicate-returns-accepted-before-action-dispatch',[
  start('one','one',{gate:'first',beforeGate:true}),gate('first'),start('duplicate','one'),wait('duplicate'),
  c('pending',{handle:'one'}),release('first'),wait('one'),snapshot,
]);
add('duplicate-returns-dispatched-and-rejects-active-conflict',[
  start('one','one',{gate:'first'}),gate('first'),start('duplicate','one'),wait('duplicate'),
  start('conflict','one',{params:{different:true}}),c('pending',{handle:'one'}),release('first'),wait('one'),snapshot,
]);
add('dropped-waiters-do-not-cancel-primary-or-duplicate',[
  start('one','one',{gate:'first'}),gate('first'),start('duplicate','one'),c('drop',{handle:'duplicate'}),
  c('drop',{handle:'one'}),release('first'),c('settle',{owner:'a',identity:identity('one'),handle:'settled'}),wait('settled'),read('one'),snapshot,
]);
add('settle-ignores-action-error-and-propagates-read-error',[
  start('one','one',{gate:'first',behavior:'lost'}),gate('first'),c('settle',{owner:'a',identity:identity('one'),handle:'settled'}),
  c('pending',{handle:'settled'}),release('first'),wait('one'),wait('settled'),clear,read('one'),
  c('readFailure'),c('settle',{owner:'a',identity:identity('one'),handle:'failed'}),wait('failed'),snapshot,
]);
add('settle-turn-filters-and-follows-later-insertions',[
  start('first','first',{method:'turn/start',params:{threadId:'thread'},gate:'first'}),gate('first'),
  start('ignored','ignored',{method:'thread/start',params:{threadId:'thread'},gate:'ignored'}),gate('ignored'),
  start('other','other',{method:'turn/start',params:{threadId:'other'},gate:'other'}),gate('other'),
  c('settleTurn',{owner:'a',threadId:'thread',handle:'settled'}),c('pending',{handle:'settled'}),
  start('later','later',{method:'turn/start',params:{threadId:'thread'},gate:'later'}),gate('later'),
  release('first'),wait('first'),c('pending',{handle:'settled'}),release('later'),wait('later'),wait('settled'),
  c('pending',{handle:'ignored'}),c('pending',{handle:'other'}),release('ignored'),release('other'),wait('ignored'),wait('other'),snapshot,
]);
for(const patch of [{callerPrincipalId:'other'},{operationId:'other'},{serviceNodeId:'other'},{phase:'invalid'}])
  add('saved-identity-or-contract-'+Object.keys(patch)[0],[start('one'),wait('one'),clear,c('patch',{id:'one',patch}),read('one'),snapshot]);
add('missing-saved-row-and-mismatched-live-hash',[
  c('memoryAccept',{owner:'a',identity:identity('unsaved')}),read('unsaved'),snapshot,
  start('one'),wait('one'),c('patch',{id:'one',patch:{requestHash:digest('different'),phase:'dispatched'}}),read('one'),snapshot,
]);
add('saved-accepted-recovers-as-unknown-without-persisting',[
  start('one'),wait('one'),clear,c('patch',{id:'one',patch:{phase:'accepted',requestId:null,epoch:null,reply:null}}),read('one'),snapshot,
]);
add('intent-without-native-epoch',[start('one'),wait('one'),snapshot],{epoch:null});
for(const requestId of [0,-7,'0','\ud800']){
  const params={identity:{serviceNodeId:'agent',epoch:'native-epoch',requestId},reply:{error:{code:-32000,message:'declined'}}};
  add('answer-request-id-'+cases.length,[c('observe',{identity:params.identity}),start('answer','answer',{method:'agent.answer',params,behavior:'answer'}),wait('answer'),snapshot]);
}
add('trusted-action-result-is-not-revalidated',[start('raw','raw',{behavior:'raw',raw:{phase:'failed',custom:true}}),wait('raw'),snapshot,clear,read('raw')]);

const fixedTime='2026-10-18T00:00:00.000Z',RealDate=Date;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[fixedTime]));}};
try{
  for(const row of cases){
    const rows=new Map(),owners=new Map(),handles=new Map(),gates=new Map(),effects=[];
    let nextFailure=null,failRead=false,writes=0;
    const store={serviceNodeId:'agent',read:async(_kind,key)=>{if(failRead){failRead=false;throw new IvyError('service_unavailable','Fixture read unavailable.');}return structuredClone(rows.get(key)??null);},
      write:async(_kind,key,value,previous)=>{
        writes++;const failure=nextFailure;nextFailure=null;
        if(failure&&!failure.afterCommit)throw new IvyError(failure.code,'Fixture write unavailable.');
        const prior=rows.get(key);if(prior?.pin.revision!==previous?.revision||prior&&prior.pin.objectId!==previous?.objectId)throw new IvyError('revision_conflict','Another owner claimed the operation.');
        const saved={value:structuredClone(value),pin:{objectId:key,revision:(prior?.pin.revision??0)+1}};rows.set(key,saved);
        if(failure)throw new IvyError(failure.code,'Fixture write unavailable.');return structuredClone(saved);
      }};
    function addOwner(name){const memory=new NativeJournal(owner,limits);if(row.epoch!==null)memory.beginEpoch(row.epoch??'native-epoch');owners.set(name,new NativeOperations(store,memory));}
    for(const name of row.owners??['a'])addOwner(name);
    function barrier(name){if(!gates.has(name)){let entered,release;const arrival=new Promise(r=>{entered=r;}),opened=new Promise(r=>{release=r;});gates.set(name,{entered,release,arrival,opened});}return gates.get(name);}
    function track(name,promise){const state={settled:false,promise:Promise.resolve(promise).then(v=>{state.settled=true;return{result:structuredClone(v??null)};},e=>{state.settled=true;return{error:IvyError.from(e).toWire()};})};handles.set(name,state);}
    row.expected=[];
    for(const step of row.commands){
      let outcome;
      try{
        const manager=owners.get(step.owner??'a');let result=null;
        switch(step.action){
          case 'owner':addOwner(step.name);break;
          case 'start':{
            const action=async()=>{
              const memory=manager.memory,key=hashJson(step.identity),saved=rows.get(key);
              if(!saved)throw Error('Native action ran without a durable claim');
              effects.push({owner:step.owner,identity:step.identity,intent:structuredClone(saved.value)});
              const epoch=memory.epoch,id=manager.requestId(step.identity);
              if(step.behavior==='reject')throw new IvyError('service_not_ready','Provider connection is closed.');
              if(step.behavior==='clearThrow'){memory.close();throw new IvyError('fixture_lost','Fixture action failed.');}
              if(step.behavior==='raw')return step.raw;
              if(step.gate&&step.beforeGate){const b=barrier(step.gate);b.entered();await b.opened;}
              if(step.behavior==='answer')memory.dispatchAnswer(step.identity,step.params.identity,step.params.reply);
              else memory.dispatch(step.identity,epoch,id);
              if(step.gate&&!step.beforeGate){const b=barrier(step.gate);b.entered();await b.opened;}
              if(step.behavior==='lost')throw new IvyError('fixture_lost','Fixture action failed.');
              if(step.behavior==='forgetThrow'){memory.forget(step.identity);throw new IvyError('fixture_lost','Fixture action failed.');}
              if(step.behavior==='unknownThrow'){memory.loseEpoch(epoch,'fixture_lost');throw new IvyError('fixture_lost','Fixture action failed.');}
              if(step.behavior==='returnLive')return memory.get(step.identity);
              const reply={result:{turn:{id:'managed-turn'},thread:'original'}};
              const value=memory.finish(step.identity,epoch,step.behavior==='answer'?step.params.identity.requestId:id,reply);
              if(step.behavior==='terminalWriteFailure')nextFailure={code:'service_unavailable',afterCommit:false};
              if(step.behavior==='terminalThrow')throw new IvyError('fixture_lost','Fixture action failed.');
              return value;
            };
            track(step.handle,manager.run(step.identity,step.method,step.params,action,step.prevent??false));break;
          }
          case 'wait':outcome=await handles.get(step.handle).promise;break;
          case 'entered':await barrier(step.gate).arrival;break;
          case 'release':barrier(step.gate).release();break;
          case 'pending':await new Promise(r=>setImmediate(r));result=!handles.get(step.handle).settled;break;
          case 'drop':handles.delete(step.handle);break;
          case 'settle':track(step.handle,manager.settle(step.identity));break;
          case 'settleTurn':track(step.handle,manager.settleTurn(step.threadId));break;
          case 'read':result=await manager.read(step.identity);break;
          case 'clear':manager.memory.close();break;
          case 'failure':nextFailure={code:step.code,afterCommit:step.afterCommit};break;
          case 'readFailure':failRead=true;break;
          case 'patch':{const key=hashJson(identity(step.id)),saved=rows.get(key);saved.value={...saved.value,...step.patch};if(step.bump)saved.pin.revision++;break;}
          case 'memoryAccept':manager.memory.accept(step.identity,'thread/start',{});break;
          case 'observe':manager.memory.observeInput(step.identity,'item/tool/requestUserInput',{threadId:'thread'});break;
          case 'snapshot':result={rows:[...rows].sort(([a],[b])=>a<b?-1:a>b?1:0).map(([key,value])=>({key,...value})),effects,writes};break;
          default:throw Error('Unknown fixture action '+step.action);
        }
        outcome??={result:structuredClone(result)};
      }catch(e){outcome={error:IvyError.from(e).toWire()};}
      row.expected.push(outcome);
    }
    for(const handle of handles.values())await handle.promise;
    for(const manager of owners.values())manager.memory.close();
  }
}finally{globalThis.Date=RealDate;}
process.stdout.write(JSON.stringify({owner,limits,fixedTime,cases}));
