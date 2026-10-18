import {readFileSync} from 'node:fs';
import {PassThrough} from 'node:stream';
import ts from 'typescript';
import {NativeJournal} from '../../dist/services/agent-manager/src/journal.js';
import {NativeOperations} from '../../dist/services/agent-manager/src/operations.js';
import {NativeRpc} from '../../dist/services/agent-manager/src/rpc.js';
import {answerNativeClock,nativeClockPrincipal} from '../../dist/services/agent-manager/src/clock.js';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';

// Execute the original private glue by extracting its AST expression, without
// importing main.ts (which starts a real service). No hand-copied dispatch body.
const source=ts.createSourceFile('main.ts',readFileSync('services/agent-manager/src/main.ts','utf8'),ts.ScriptTarget.Latest,true);
function expression(kind,name){const matches=[];function visit(node){if(kind==='variable'&&ts.isVariableDeclaration(node)&&ts.isIdentifier(node.name)&&node.name.text===name)matches.push(node.initializer);if(kind==='property'&&ts.isPropertyAssignment(node)&&node.name.getText(source).replaceAll('"','')===name)matches.push(node.initializer);ts.forEachChild(node,visit);}visit(source);if(matches.length!==1)throw Error('Expected one original expression: '+name);return matches[0].getText(source);}
const expressions=Object.fromEntries([['variable','identity'],['variable','available'],['variable','executeNative'],['property','agent.answer'],['property','onRequest'],['property','onNotification'],['property','onClose']].map(([kind,name])=>[name,expression(kind,name)]));
function evaluate(text,env){const js=ts.transpileModule('const value = '+text+';',{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ESNext}}).outputText;return Function('env','IvyError','requireThat','answerNativeClock','nativeClockPrincipal','with(env){'+js+';return value;}')(env,IvyError,requireThat,answerNativeClock,nativeClockPrincipal);}
const catalog=JSON.parse(readFileSync('specs/native/codex-0.154.0/catalog.json','utf8'));
const limits={maxOperations:100,maxJournalBytes:128*1024*1024,maxPendingInputs:16,maxNotificationBytes:1048576};
const c=(action,fields={})=>({action,...fields});
const context=(operationId='one',callerPrincipalId='caller')=>({operationId,callerPrincipalId});
const invoke=(handle='one',extra={})=>c('invoke',{handle,context:context(handle),method:'account/logout',params:null,...extra});
const wait=handle=>c('wait',{handle}),sent=c('sent'),snapshot=c('snapshot'),drain=c('drain');
const emit=frame=>c('emit',{frame}),reply=result=>c('reply',{reply:{result}});
const inputIdentity=requestId=>({serviceNodeId:'agent',epoch:'live',requestId});
const input=(requestId,params={threadId:'thread'},method='item/tool/requestUserInput')=>emit({id:requestId,method,params});
const answer=(handle,requestId,extra={})=>c('answer',{handle,context:context(handle),input:{operationId:handle,identity:inputIdentity(requestId),reply:{error:{code:-32000,message:'declined'}}},...extra});
const resolved=(requestId,threadId='thread')=>emit({method:'serverRequest/resolved',params:{requestId,threadId}});
const cases=[];const add=(name,commands,extra={})=>cases.push({name,commands,...extra});
add('durable-request-reply-replay',[invoke(),sent,reply({turn:{id:'turn'}}),wait('one'),drain,snapshot,invoke('replay',{context:context('one')}),wait('replay'),snapshot]);
add('native-error-remains-original',[invoke(),sent,c('reply',{reply:{error:{code:-32602,message:'original',data:{untouched:['\ud800',null]}}}}),wait('one'),drain,snapshot]);
add('active-duplicate-does-not-write-again',[invoke(),sent,invoke('duplicate',{context:context('one')}),wait('duplicate'),reply(null),wait('one'),drain,snapshot]);
add('claim-write-failure-writes-no-native-bytes',[c('writeFailure'),invoke(),wait('one'),snapshot]);
for(const method of ['missing/native/method','account/logout'])add('before-dispatch-refusal-'+method,[c(method==='account/logout'?'closing':'noop'),invoke('one',{method}),wait('one'),drain,snapshot]);
add('close-after-dispatch-persists-unknown',[invoke(),sent,c('close',{code:'fixture_lost'}),wait('one'),drain,snapshot]);
add('malformed-reply-closes-with-original-uncertainty',[invoke(),sent,emit({id:'$last',result:{},error:{code:1,message:'both'}}),wait('one'),drain,snapshot]);
add('inactive-answer-resolved-reconciles-terminal-receipt',[input(7),answer('answer',7),sent,wait('answer'),snapshot,resolved(7),drain,snapshot]);
add('typed-inputs-remain-distinct',[input(7),input('7'),answer('number',7),sent,wait('number'),answer('string','7'),sent,wait('string'),resolved(7),resolved('7'),drain,snapshot]);
add('answering-input-loses-owner',[input('lost'),answer('answer','lost'),sent,wait('answer'),c('close',{code:'fixture_lost'}),drain,snapshot]);
add('missing-input-is-retained-unexecuted',[answer('missing','unknown'),wait('missing'),drain,snapshot]);
add('host-clock-is-not-a-management-answer',[input('clock',{threadId:'thread'},'currentTime/read'),answer('answer','clock'),wait('answer'),drain,snapshot]);
add('outer-answer-identity-mismatch',[answer('bad',7,{context:context('different')}),snapshot]);
add('terminal-reconciliation-error-is-reported',[input('question'),answer('answer','question'),sent,wait('answer'),c('writeFailure'),resolved('question'),drain,snapshot,c('read',{identity:context('answer')}),snapshot]);
add('invalid-null-resolution-keeps-published-event-before-close',[input('question'),emit({method:'serverRequest/resolved',params:null}),drain,snapshot]);
add('untyped-resolution-field-retains-original-null-thread-rule',[input('question',{}),resolved('question',17),drain,snapshot]);
add('mismatched-resolution-thread-closes',[input('question'),answer('answer','question'),sent,wait('answer'),resolved('question','different'),drain,snapshot]);
for(const operationId of [null,''])add('missing-identity-'+String(operationId),[invoke('one',{context:context(operationId)}),wait('one'),snapshot]);
add('reserved-clock-principal',[invoke('one',{context:context('one',nativeClockPrincipal)}),wait('one'),snapshot]);
for(const requestId of [0,'0','\ud800'])add('host-clock-'+cases.length,[input(requestId,{threadId:'thread'},'currentTime/read'),c('clock'),sent,c('clock'),resolved(requestId),drain,snapshot,c('clock'),snapshot]);
add('host-clock-floors-negative-milliseconds',[input(1,{threadId:'thread'},'currentTime/read'),c('clock'),sent,snapshot],{time:'1969-12-31T23:59:59.999Z'});
add('host-clock-extended-year',[input(1,{threadId:'thread'},'currentTime/read'),c('clock'),sent,snapshot],{time:'+275760-09-13T00:00:00.000Z'});
add('host-clock-rejects-substituted-observation',[input(1,{threadId:'thread'},'currentTime/read'),c('clock',{patch:{observedAt:'2030-01-01T00:00:00.000Z'}}),snapshot]);
add('clock-does-not-answer-other-methods',[input(1),c('clock'),snapshot]);
add('input-admission-failure-writes-no-clock-bytes',[input(1,{threadId:'thread'},'currentTime/read'),c('clock'),snapshot],{maxOperations:0});
add('clock-admission-failure-writes-no-bytes',[input(1,{threadId:'thread'},'currentTime/read'),c('occupy'),c('clock'),snapshot],{maxOperations:1});

const RealDate=Date,results=[];
try{
 for(const row of cases){
  globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[row.time??'2026-10-18T00:00:00.000Z']));}};
  const memory=new NativeJournal({serviceNodeId:'agent',hostId:'host',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash},{...limits,maxOperations:row.maxOperations??limits.maxOperations});memory.beginEpoch('live');
  const rows=new Map(),frames=[],inputs=[],notifications=[],closes=[],errors=[],handles=new Map(),work=new Set(),arrivals=[];
  let writeFailure=false,last=null,cursor=0;
  const store={serviceNodeId:'agent',read:async(_kind,key)=>structuredClone(rows.get(key)??null),write:async(_kind,key,value,previous)=>{
   if(writeFailure){writeFailure=false;throw new IvyError('service_unavailable','Fixture write unavailable.');}
   const prior=rows.get(key);if(prior?.pin.revision!==previous?.revision)throw new IvyError('revision_conflict','Another owner claimed the operation.');
   const saved={value:structuredClone(value),pin:{objectId:key,revision:(prior?.pin.revision??0)+1}};rows.set(key,saved);return structuredClone(saved);
  }};
  const operations=new NativeOperations(store,memory),inputStream=new PassThrough(),output=new PassThrough();
  const track=promise=>{work.add(promise);void promise.then(()=>work.delete(promise),()=>work.delete(promise));return promise;};
  memory.onTerminal=identity=>{if(identity.callerPrincipalId!==nativeClockPrincipal)void track(operations.settle(identity)).catch(e=>errors.push(e.code));};
  const env={journal:memory,operations,owner:{epoch:'live',catalog},closing:false,service:null,native:null,config:{serviceNodeId:'agent'},startupClocks:[],requestThreadRefresh:null,requestProjectRefresh:null,
   noticePolicy:{observe(){},isInternal(){return false;}},notifications:{observe(epoch,method,params){notifications.push({method,params:structuredClone(params)});return{};}}};
  for(const name of ['identity','available','executeNative'])env[name]=evaluate(expressions[name],env);
  const onRequest=evaluate(expressions.onRequest,env),onNotification=evaluate(expressions.onNotification,env),onClose=evaluate(expressions.onClose,env),answer=evaluate(expressions['agent.answer'],env);
  const rpc=new NativeRpc(catalog,inputStream,output,{onRequest:value=>{onRequest('live',value);inputs.push(memory.input(inputIdentity(value.id)));},onNotification:value=>onNotification('live',value),onClose:code=>{onClose('live',code);closes.push(code);}});env.owner.rpc=rpc;
  inputStream.on('data',bytes=>{frames.push(JSON.parse(bytes.toString('utf8')));arrivals.shift()?.();});
  const capture=async action=>{try{return{result:structuredClone(await action()??null)};}catch(e){return{error:IvyError.from(e).toWire()};}};
  function start(name,action){const promise=action();handles.set(name,capture(()=>promise));}
  row.expected=[];
  for(const step of row.commands){let result;
   if(step.action==='wait')result=await handles.get(step.handle);
   else result=await capture(async()=>{
    switch(step.action){
     case 'invoke':start(step.handle,()=>env.executeNative(step.method,step.params,step.context));break;
     case 'answer':start(step.handle,()=>answer(step.input,step.context));break;
     case 'sent':if(cursor===frames.length)await new Promise(resolve=>arrivals.push(resolve));last=frames[cursor++];return last;
     case 'reply':output.write(JSON.stringify({id:last.id,...step.reply})+'\n');break;
     case 'emit':{const frame=structuredClone(step.frame);if(frame.id==='$last')frame.id=last.id;output.write(JSON.stringify(frame)+'\n');break;}
     case 'close':rpc.close(step.code);break;
     case 'drain':while(work.size)await Promise.allSettled([...work]);break;
     case 'writeFailure':writeFailure=true;break;
     case 'closing':env.closing=true;break;
     case 'noop':break;
     case 'read':return operations.read(step.identity);
     case 'clock':answerNativeClock(memory,rpc,{...inputs.at(-1),...step.patch});break;
     case 'occupy':memory.accept({callerPrincipalId:'fixture',operationId:'occupied'},'turn/interrupt',{});break;
     case 'snapshot':return{rows:[...rows].sort(([a],[b])=>a<b?-1:a>b?1:0).map(([key,value])=>({key,...value})),inputs:memory.inputs({includeExpired:true}),clockOperations:inputs.filter(value=>value.method==='currentTime/read').map(value=>memory.get({callerPrincipalId:nativeClockPrincipal,operationId:'clock:'+hashJson(value.identity).slice(7)})),frames,notifications,closes,errors,epoch:memory.epoch};
     default:throw Error('Unknown fixture action '+step.action);
    }
    return null;
   });
   row.expected.push(result);
  }
  while(work.size)await Promise.allSettled([...work]);rpc.close('fixture_stop');inputStream.destroy();output.destroy();memory.close();
  results.push(row);
 }
}finally{globalThis.Date=RealDate;}
process.stdout.write(JSON.stringify({catalogVersion:catalog.version,catalogHash:hashJson(catalog),limits,extracted:expressions,cases:results}));
