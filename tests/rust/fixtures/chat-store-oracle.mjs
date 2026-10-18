import {existsSync,readFileSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
const contracts=new URL('../../../packages/contracts/src/',import.meta.url);
const sdk=new URL('../../../packages/sdk/src/',import.meta.url);
const data=source=>'data:text/javascript;base64,'+Buffer.from(source).toString('base64');
const exports=data([
 `export {canonical,hashJson} from '${new URL('canonical.ts',contracts)}';`,
 `export {IvyError,requireThat} from '${new URL('errors.ts',contracts)}';`,
 `export {parseOperationId} from '${new URL('operation-id.ts',contracts)}';`,
 `export {hasUnresolvedOutcome} from '${new URL('receipt-archive.ts',sdk)}';`,
 `export {bundledSchema} from '${new URL('bundle.ts',contracts)}';`,
 `export {validateChat} from '${new URL('chat-validation.ts',contracts)}';`,
].join('\n'));
registerHooks({resolve(s,c,n){
 if(s==='../../../packages/sdk/src/node.js')return {url:exports,shortCircuit:true};
 if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}
 return n(s,c);
},load(u,c,n){
 if(u.endsWith('.ts'))return {format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(u),'utf8'),{mode:'transform'}),shortCircuit:true};
 return n(u,c);
}});
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const now=1790000000123,iso=new Date(now).toISOString(),RealDate=Date;
let clock=now;
globalThis.Date=class extends RealDate {constructor(...args){super(...(args.length?args:[clock]));}static now(){return clock;}};
const create=(alias,key,value,parent='root',name=alias)=>({action:'create',alias,key:'chat-bridge/'+key,value,parent,name});
const update=(alias,key,value,revision)=>({action:'update',alias,key:'chat-bridge/'+key,value,...(revision===undefined?{}:{revision})});
const named=(alias,key='operation',name=alias,parent='root')=>({action:'named',alias,key:'chat-bridge/'+key,name,parent});
const expire={action:'expire'},limit=count=>({action:'limit',count});
const op=(nonce,extra={})=>({operationId:`chat:${now-1000}:${nonce}`,phase:'succeeded',updatedAt:iso,...extra});
const cases=[];
cases.push({label:'sibling-retirement-and-replay-fence',actions:[
 create('done','operation',op('done')),create('pending','operation',op('pending',{phase:'accepted'})),
 ...['done','pending'].flatMap(alias=>[create('offer-'+alias,'offer',{replies:[]},'root','Chat offer $id:'+alias),create('ack-'+alias,'acknowledgement',{},'root','Chat acknowledgement $id:'+alias)]),
 limit(3),expire,...['done','pending'].flatMap(alias=>[named(alias),named('offer-'+alias,'offer','Chat offer $id:'+alias),named('ack-'+alias,'acknowledgement','Chat acknowledgement $id:'+alias)]),
 {action:'validate',id:`chat:${now-1000}:done`},create('replay','operation',op('done')),expire,named('pending')
]});
cases.push({label:'queued-input-and-notice-producers',actions:[
 create('main','main',{queue:[{inputId:'waiting',identity:{senderPrincipalId:'producer',messageId:`chat:${now-1000}:notice`}}]}),
 ...['send','cancel'].map(action=>create(action,'operation',op(action,{outcome:{action,input:{object:{objectId:'waiting',revision:1}}}}))),
 ...['succeeded','failed'].map(phase=>create(phase,'operation',op('notice',{phase,callerPrincipalId:'producer',request:{action:'notify'}}))),
 limit(1),expire,...['send','cancel','succeeded','failed'].map(a=>named(a)),update('main','main',{queue:[]}),expire,...['send','cancel','succeeded','failed'].map(a=>named(a))
]});
cases.push({label:'partial-offer-pending-ack-and-unknown',actions:[
 create('receive','operation',op('receive',{request:{action:'receive'}})),create('offer','offer',{replies:[{objectId:'one',revision:1},{objectId:'two',revision:1}]},'root','Chat offer $id:receive'),
 create('unknown','operation',op('unknown',{phase:'outcome_unknown'})),
 create('ack','operation',op('ack',{phase:'accepted',request:{action:'acknowledge',offerId:'$id:offer',replyIds:['one','two']}})),
 limit(2),expire,{action:'confirm',alias:'offer',replies:['one']},expire,named('receive'),{action:'confirm',alias:'offer',replies:['two']},expire,named('receive'),
 limit(10),update('ack','operation',op('ack',{request:{action:'acknowledge',offerId:'$id:offer',replyIds:['one','two']}})),limit(1),expire,named('receive'),named('unknown')
]});
cases.push({label:'delivery-is-separate-from-turn-completion',actions:[
 create('notice','operation',op('notice',{outcome:{action:'notify',reply:{object:{objectId:'reply',revision:1}}}})),create('uncertain','operation',op('uncertain',{error:{code:'native_connection_lost'}})),
 limit(1),expire,named('notice'),{action:'delivered',id:'reply'},expire,named('notice'),named('uncertain')
]});
cases.push({label:'rollback-after-eviction-and-live-byte-reservation',actions:[
 create('old','operation',op('old')),create('live','operation',op('live',{phase:'accepted'})),limit(1),
 create('too-large','main',{sequence:1}),named('old'),named('live'),named('too-large','main'),
 {action:'limit',count:100,bytes:1048576},create('next-live','operation',op('next-live',{phase:'accepted'})),named('next-live'),named('old'),named('live')
]});
cases.push({label:'pending-revisions-and-collection-window',actions:[
 create('main','main',{sequence:0}),create('resume','main',{sequence:0}),
 create('change','operation',op('change',{phase:'accepted',request:{action:'createMain',expectedMainRevision:1,resumeMain:{objectId:'$id:resume',revision:1}}})),
 update('main','main',{pendingAction:{objectId:'$id:change',revision:1}}),update('resume','main',{sequence:1}),
 create('collection','collection',{sequence:0}),...Array.from({length:35},(_,i)=>update('collection','collection',{sequence:i+1})),
 {action:'read',alias:'main',key:'chat-bridge/main',revision:1},{action:'read',alias:'resume',key:'chat-bridge/main',revision:1},
 {action:'read',alias:'collection',key:'chat-bridge/collection',revision:1},{action:'read',alias:'collection',key:'chat-bridge/collection',revision:5},
 update('main','main',{sequence:2}),expire,{action:'read',alias:'resume',key:'chat-bridge/main',revision:1}
]});
cases.push({label:'terminal-input-child-cleanup-and-blob-identity',actions:[
 create('child','collection',{},'input'),{action:'blob',alias:'blob',name:'bytes',parent:'$id:child',bytes:[0,1,255]},
 {action:'blob',alias:'blob-again',name:'bytes',parent:'$id:child',bytes:[0,1,255]},
 {action:'blob',alias:'bad',name:'bytes',parent:'$id:child',bytes:[2]},limit(0),{action:'complete',id:'input'},named('child','collection','child','input')
]});
cases.push({label:'identity-scope-and-optimistic-write-conflicts',actions:[
 create('one','main',{text:'opaque\ud800',n:1},null,'name'),create('same','main',{n:1,text:'opaque\ud800'},null,'name'),
 create('different','main',{n:2},null,'name'),update('one','main',{n:2},9),
 {action:'read',alias:'one',key:'chat-bridge/main',parent:'other'},
 {action:'read',alias:'one',key:'chat-bridge/main',parent:null,revision:2},
 {action:'read',alias:'one',key:'chat-bridge/main',parent:null,revision:1},
 {action:'validate',id:`other:${now}:a`,retained:true},{action:'validate',id:`chat:${now+60001}:a`},{action:'validate',id:`chat:${now+60001}:a`,retained:true}
]});
const complete=at=>({active:false,resolvedAt:at,alert:{confirmed:true},recovery:{confirmed:true}});
cases.push({label:'diagnostic-history-and-notice-input-expiry',actions:[
 {action:'diagnostic',count:2,value:{'10':complete(1),'2':complete(1),old:complete(0),active:{...complete(0),active:true},unconfirmed:{...complete(0),recovery:{confirmed:false}}}},
 {action:'diagnostic',count:100,bytes:2,value:{done:complete(1)}},
 {action:'notice-input',id:'fixture'}, {action:'notice-input',id:'fixture',at:now+1000},
 {action:'is-notice-input',id:'fixture',at:now+7*86400000-1},
 {action:'is-notice-input',id:'fixture',at:now+7*86400000},
 {action:'expire',at:now+7*86400000}, {action:'is-notice-input',id:'fixture',at:now+7*86400000}
]});
const {mutation}=await import('../../../services/chat-bridge/src/store.ts');
for(const scenario of cases){
 clock=now;
 const client={async request(method,args){if(method==='system.status')return {runtimeEpoch:'chat'};if(method==='objects.write')return {object:{id:args.objectId},revision:{revision:2}};throw Error('Unexpected '+method);}};
 const store=new ChatStore(client,'root');store.setRuntimeEpoch('chat');
 const pins=new Map();
 const resolve=v=>typeof v==='string'?[...pins].sort((a,b)=>b[0].length-a[0].length).reduce((s,[a,p])=>s.replaceAll('$id:'+a,p.objectId),v):Array.isArray(v)?v.map(resolve):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,resolve(v)])):v;
 const normalize=v=>typeof v==='string'?[...pins].reduce((s,[a,p])=>s.replaceAll(p.objectId,'$id:'+a),v):Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)])):v;
 scenario.expected=[];
 for(const original of scenario.actions){const a=resolve(original);let result=null;clock=a.at??now;
  try{switch(a.action){
   case 'create':case 'update':{const pin=await store.write(a.key,a.value,'fixture',a.action==='create'?{create:{parentId:a.parent,name:a.name}}:{objectId:pins.get(a.alias).objectId,expectedRevision:a.revision??pins.get(a.alias).revision});pins.set(a.alias,pin);result=pin;break;}
   case 'named':{const doc=await store.named(a.key,a.name,a.parent??(a.parent===null?null:'root'));result=doc?{pin:doc.pin,value:doc.value}:null;break;}
   case 'read':{const doc=await store.read(a.key,{objectId:pins.get(a.alias).objectId,revision:a.revision??pins.get(a.alias).revision},a.parent===undefined?'root':a.parent);result={pin:doc.pin,value:doc.value};break;}
   case 'limit':store.limits.count=a.count;if(a.bytes!==undefined)store.limits.bytes=a.bytes;break;
   case 'expire':store.expireOperations(clock,true);break;
   case 'confirm':store.confirmOffer(pins.get(a.alias).objectId,a.replies);break;
   case 'delivered':store.rememberNoticeDelivery(a.id);break;
   case 'validate':await store.validateOperationId(a.id,now,a.retained??false);break;
   case 'blob':{const pin=store.saveLocalBlob('chat-bridge/evidence-chunk',a.name,a.parent,Buffer.from(a.bytes));pins.set(a.alias,pin);result=pin;break;}
   case 'complete':await store.write('chat-bridge/input',{payload:{images:[]},state:'completed',finishedAt:iso},'complete',{objectId:a.id,expectedRevision:1});break;
   case 'diagnostic':store.diagnosticHistoryLimits.count=a.count;if(a.bytes!==undefined)store.diagnosticHistoryLimits.bytes=a.bytes;store.saveDiagnosticNoticeState('scope',a.value);result={mutated:a.value,saved:store.diagnosticNoticeState('scope')};break;
   case 'notice-input':store.rememberNoticeInput(a.id);break;
   case 'is-notice-input':result=store.isNoticeInput(mutation(a.id,'native-user-message'));break;
   default:throw Error('Bad fixture action '+a.action);
  }}catch(error){const e=IvyError.from(error);result={error:{code:e.code,message:e.message,outcome:e.outcome}};}
  scenario.expected.push(normalize(result));
 }
 store.close();
}
process.stdout.write(JSON.stringify({now,iso,cases}));
