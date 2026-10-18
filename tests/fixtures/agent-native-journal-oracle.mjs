import {NativeJournal} from '../../dist/services/agent-manager/src/journal.js';
import {nativeRequestFrameBytes,nativeFrameBytes,nativeAnswerFrameBytes} from '../../dist/services/agent-manager/src/limits.js';
import {digest,hashJson} from '../../dist/packages/contracts/src/canonical.js';
import {encodeJson} from '../../dist/packages/contracts/src/canonical-json.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
const owner={hostId:'fixture-host',serviceNodeId:'agent',nativeVersion:'0.154.0',nativeExecutableHash:digest('native fixture')};
const defaults={maxOperations:100,maxJournalBytes:128*1024*1024,maxPendingInputs:16,maxNotificationBytes:1048576};
const key=(operationId,callerPrincipalId='caller')=>({callerPrincipalId,operationId});
const input=(requestId,epoch='live',serviceNodeId='agent')=>({serviceNodeId,epoch,requestId});
const command=(action,fields={})=>({action,...fields});
const accept=(id,method='thread/start',params={})=>command('accept',{identity:key(id),method,params});
const dispatch=(id,requestId=1,epoch='live')=>command('dispatch',{identity:key(id),epoch,requestId});
const finish=(id,reply={result:{}},requestId=1,epoch='live')=>command('finish',{identity:key(id),epoch,requestId,reply});
const observe=(requestId,params={threadId:'thread',turnId:'turn'},method='item/tool/requestUserInput')=>command('observeInput',{identity:input(requestId),method,params});
const answer=(id,requestId,reply)=>accept(id,'agent.answer',{identity:input(requestId),reply});
const sendAnswer=(id,requestId,reply)=>command('dispatchAnswer',{operation:key(id),identity:input(requestId),reply});
const cases=[];
function add(name,commands,limits=defaults,extra={}){cases.push({name,limits,owner,...extra,commands});}
const begin=command('beginEpoch',{epoch:'live'}),ok={result:{answers:{q:{answers:['first']}}}};
add('operations-and-replay',[begin,accept('one'),accept('one'),command('prevent',{identity:key('one'),method:'thread/start',params:{}}),accept('one','thread/start',{other:true}),dispatch('one'),dispatch('one',2),finish('one',{result:{}},'1'),command('rejectBeforeDispatch',{identity:key('one'),code:'no'}),finish('one',{error:{code:-32602,message:'Native original',data:{untouched:[false,null,'\ud800']}}}),finish('one'),command('get',{identity:key('one','other')}),command('accept',{identity:key('one','other'),method:'thread/start',params:{}}),command('retainedInHive',{identity:key('one')}),accept('after-compaction'),command('get',{identity:key('one')}),command('forget',{identity:key('after-compaction')}),command('get',{identity:key('after-compaction')}),command('reserveNotificationSequence'),command('inventoryRevision',{kind:'thread',minimum:0}),command('inventoryRevision',{kind:'thread',minimum:7}),command('inventoryRevision',{kind:'project',minimum:2}),command('close'),command('inventoryRevision',{kind:'thread',minimum:0})]);
add('epochs-and-late-results',[begin,accept('sent','turn/start',{threadId:'thread'}),accept('unsent'),dispatch('sent',-7),command('loseEpoch',{epoch:'other',code:'lost'}),command('loseEpoch',{epoch:'live',code:'connection_lost'}),command('get',{identity:key('sent')}),command('get',{identity:key('unsent')}),command('beginEpoch',{epoch:'replacement'}),command('beginEpoch',{epoch:'replacement'}),finish('sent',{result:{}},-7,'live'),accept('sent','turn/start',{threadId:'thread'}),command('close'),command('get',{identity:key('sent')})]);
add('count-cleanup-reservation',[begin,accept('ordinary'),accept('blocked'),accept('interrupt','turn/interrupt'),accept('unsubscribe','thread/unsubscribe'),command('prevent',{identity:key('prevented'),method:'thread/start',params:{}}),accept('last','turn/interrupt'),command('prevent',{identity:key('full'),method:'thread/start',params:{}}),accept('ordinary'),command('retainedInHive',{identity:key('prevented')}),accept('compact','turn/interrupt')],{...defaults,maxOperations:5});
add('byte-cleanup-reservation',[begin,accept('one'),accept('no-regular'),accept('cleanup','turn/interrupt'),command('prevent',{identity:key('small'),method:'thread/start',params:{}}),command('rejectBeforeDispatch',{identity:key('one'),code:'refused'}),command('retainedInHive',{identity:key('one')}),accept('after')],{...defaults,maxJournalBytes:2*nativeFrameBytes+1024*1024});
add('input-ownership',[begin,observe(1),observe(1),observe('1',{conversationId:'conversation',turnId:7}),observe(1,{}),command('input',{identity:input(1)}),command('inputs',{query:{}}),answer('answer',1,ok),sendAnswer('answer',1,ok),sendAnswer('answer',1,ok),answer('rival',1,ok),sendAnswer('rival',1,ok),command('resolveInput',{epoch:'live',requestId:1,threadId:'wrong'}),command('resolveInput',{epoch:'live',requestId:1,threadId:'thread'}),command('resolveInput',{epoch:'live',requestId:1,threadId:'thread'}),command('get',{identity:key('answer')}),command('inputs',{query:{identity:input(1)}}),command('inputs',{query:{includeExpired:true,limit:1}}),command('inputs',{query:{}}),command('resolveInput',{epoch:'live',requestId:99,threadId:'thread'}),observe(2,{threadId:7,conversationId:'not-fallback'}),command('resolveInput',{epoch:'live',requestId:2,threadId:'anything'})]);
add('input-expiry-and-history',[begin,observe(7),observe('7'),observe(8),answer('answer',7,ok),sendAnswer('answer',7,ok),command('loseEpoch',{epoch:'live',code:'lost'}),command('input',{identity:input(7)}),command('input',{identity:input('7')}),command('beginEpoch',{epoch:'new'}),command('observeInput',{identity:input(7,'new'),method:'currentTime/read',params:{threadId:'new-thread'}}),command('inputs',{query:{includeExpired:true,limit:2}}),command('resolveInput',{epoch:'live',requestId:7,threadId:'thread'}),command('inputs',{query:{identity:input(7)}})],{...defaults,maxPendingInputs:2});
add('invalid-identities-and-replies',[command('get',{identity:key('')}),command('get',{identity:key('x','')}),command('beginEpoch',{epoch:''}),begin,command('observeInput',{identity:input(1,'old'),method:'x',params:{}}),command('observeInput',{identity:input(1,'live','other'),method:'x',params:{}}),command('observeInput',{identity:{epoch:'live',serviceNodeId:'agent'},method:'x',params:{}}),command('observeInput',{identity:input(null),method:'x',params:{}}),command('input',{identity:input(null)}),command('inputs',{query:{limit:0}}),accept('answer-null','agent.answer',null),accept('answer-array','agent.answer',[]),accept('answer-empty','agent.answer',{}),accept('one'),dispatch('one'),finish('one',[]),finish('one',{result:{},error:{code:1,message:'both'}}),finish('one',{error:{code:'bad',message:'invalid'}}),finish('one',{result:'accepted',id:'overridden','\ud800':true}),command('get',{identity:key('one')})]);
add('unicode-and-clone-isolation',[begin,command('accept',{identity:key('é','\ud800'),method:'method/😀',params:{'\ud800':'\udc00','2':false,'1':true,text:'line\r\n😀'}}),command('accept',{identity:key('é','\ud800'),method:'method/😀',params:{text:'line\r\n😀','1':true,'2':false,'\ud800':'\udc00'}}),command('mutateRetrieved',{identity:key('é','\ud800')}),command('get',{identity:key('é','\ud800')}),command('dispatch',{identity:key('é','\ud800'),epoch:'live',requestId:'\ud800'}),command('finish',{identity:key('é','\ud800'),epoch:'live',requestId:'\ud800',reply:{result:{'\ud800':'kept'}}})]);
add('terminal-callback-order',[begin,command('callback',{mode:'mutate'}),accept('one'),command('rejectBeforeDispatch',{identity:key('one'),code:'first'}),command('get',{identity:key('one')}),command('callback',{mode:'throw'}),accept('two'),command('rejectBeforeDispatch',{identity:key('two'),code:'second'}),command('get',{identity:key('two')}),command('callback',{mode:'record'}),accept('third'),command('beginEpoch',{epoch:'new'}),command('close'),accept('after-close'),command('rejectBeforeDispatch',{identity:key('after-close'),code:'closed'})]);
add('observation-fifo-and-clock-compaction',[begin,command('sweepObservations',{count:260}),command('get',{identity:key('sweep-0')}),command('observedMethods'),accept('same-method','method/4'),command('rejectBeforeDispatch',{identity:key('same-method'),code:'again'}),accept('new-method','method/new'),command('rejectBeforeDispatch',{identity:key('new-method'),code:'new'}),command('observedMethods'),command('accept',{identity:key('clock','ivy-native-clock'),method:'currentTime/read',params:{}}),command('rejectBeforeDispatch',{identity:key('clock','ivy-native-clock'),code:'clock'}),accept('prune-clock'),command('get',{identity:key('clock','ivy-native-clock')})]);
for(const invalid of [{...owner,serviceNodeId:''},{...owner,nativeExecutableHash:'bad'}])add('constructor-'+cases.length,[],defaults,{owner:invalid});
const placeholder='00000000-0000-0000-0000-000000000000';
const requestSize=nativeRequestFrameBytes-Buffer.byteLength(encodeJson({id:placeholder,method:'turn/start',params:{payload:''}}));
const replySize=nativeFrameBytes-Buffer.byteLength(encodeJson({id:'large',result:{payload:''}}));
const fill=(count,char='x')=>({$fill:char,count});
add('full-request-and-reply',[begin,accept('large','turn/start',{payload:fill(requestSize,'q')}),accept('oversized','turn/start',{payload:fill(requestSize+1,'q')}),dispatch('large','large'),finish('large',{result:{payload:fill(replySize+1,'r')}},'large'),finish('large',{result:{payload:fill(replySize,'r')}},'large'),command('get',{identity:key('large')})]);
const incomingSize=nativeFrameBytes-Buffer.byteLength(encodeJson({id:'question',method:'item/tool/requestUserInput',params:{threadId:'thread',payload:''}}));
const answerSize=nativeAnswerFrameBytes-Buffer.byteLength(encodeJson({id:'question',error:{code:-32000,message:'boundary',data:''}}));
const largeReply={error:{code:-32000,message:'boundary',data:fill(answerSize,'r')}};
add('maximum-input-and-answer',[begin,observe('question',{threadId:'thread',payload:fill(incomingSize,'q')}),observe('overflow',{threadId:'thread',payload:fill(incomingSize+1,'q')}),command('inputs',{query:{}}),answer('too-large','question',{error:{...largeReply.error,data:fill(answerSize+1,'r')}}),answer('answer','question',largeReply),sendAnswer('answer','question',largeReply),command('inputs',{query:{}}),command('loseEpoch',{epoch:'live',code:'lost'}),command('inputs',{query:{includeExpired:true}})]);
add('page-budget',[begin,...Array.from({length:4},(_,i)=>observe(i,{threadId:'thread',payload:fill(8*1024*1024,'q')})),command('inputs',{query:{}}),command('inputs',{query:{limit:1}})],{...defaults,maxJournalBytes:256*1024*1024});

function materialize(value){
  if(Array.isArray(value))return value.map(materialize);
  if(value&&typeof value==='object'){if('$fill'in value)return value.$fill.repeat(value.count);return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,materialize(v)]));}return value;
}
function capture(value){
  if(value===undefined)value=null;
  const text=encodeJson(value);return Buffer.byteLength(text)>4096?{encodedBytes:Buffer.byteLength(text),hash:hashJson(value)}:{value};
}
const RealDate=Date,base=Date.parse('2026-10-18T00:00:00.000Z');
for(const row of cases){
  let tick=0,callbacks=[];globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[base+tick++]));}};
  try{
    let journal;try{journal=new NativeJournal(row.owner,row.limits);}catch(e){row.constructorError=IvyError.from(e).toWire();continue;}
    row.expected=[];
    for(const descriptor of row.commands){const c=materialize(descriptor);let result;
      try{
        switch(c.action){
          case 'accept':result=journal.accept(c.identity,c.method,c.params);break;
          case 'prevent':result=journal.prevent(c.identity,c.method,c.params);break;
          case 'beginEpoch':result=journal.beginEpoch(c.epoch);break;
          case 'loseEpoch':result=journal.loseEpoch(c.epoch,c.code);break;
          case 'dispatch':result=journal.dispatch(c.identity,c.epoch,c.requestId);break;
          case 'finish':result=journal.finish(c.identity,c.epoch,c.requestId,c.reply);break;
          case 'rejectBeforeDispatch':result=journal.rejectBeforeDispatch(c.identity,c.code);break;
          case 'get':result=journal.get(c.identity);break;
          case 'forget':result=journal.forget(c.identity);break;
          case 'retainedInHive':result=journal.retainedInHive(c.identity);break;
          case 'observeInput':result=journal.observeInput(c.identity,c.method,c.params);break;
          case 'input':result=journal.input(c.identity);break;
          case 'dispatchAnswer':result=journal.dispatchAnswer(c.operation,c.identity,c.reply);break;
          case 'resolveInput':result=journal.resolveInput(c.epoch,c.requestId,c.threadId);break;
          case 'inputs':result=journal.inputs(c.query);break;
          case 'observedMethods':result=journal.observedMethods();break;
          case 'reserveNotificationSequence':result=journal.reserveNotificationSequence();break;
          case 'inventoryRevision':result=journal.inventoryRevision(c.kind,c.minimum);break;
          case 'close':result=journal.close();break;
          case 'mutateRetrieved':{const value=journal.get(c.identity);value.params={foreign:true};value.phase='foreign';result=null;break;}
          case 'callback':journal.onTerminal=value=>{callbacks.push(capture(value));if(c.mode==='mutate')value.callbackTouched=true;if(c.mode==='throw')throw new IvyError('fixture_callback','fixture callback failure','unknown');};break;
          case 'sweepObservations':for(let i=0;i<c.count;i++){const identity=key('sweep-'+i);journal.accept(identity,'method/'+i,{});journal.rejectBeforeDispatch(identity,'refused');journal.retainedInHive(identity);}result=c.count;break;
          default:throw Error('Unknown action '+c.action);
        }
        result={result:capture(result)};
      }catch(e){result={error:IvyError.from(e).toWire()};}
      row.expected.push({outcome:result,status:journal.status(),epoch:journal.epoch,pending:journal.pendingInputCount(),observed:capture(journal.observedMethods()),callbacks:capture(callbacks),tick});
    }
    journal.close();
  }finally{globalThis.Date=RealDate;}
}
process.stdout.write(JSON.stringify({owner,defaults,base,frameLimits:{request:nativeRequestFrameBytes,receive:nativeFrameBytes,answer:nativeAnswerFrameBytes},cases}));
