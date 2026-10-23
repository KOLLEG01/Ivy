import './schema-oracle-dependencies.mjs';
import {existsSync,readFileSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {readNativeTurnContents,verifyNativeTurnContents}=await import('../../packages/sdk/src/native-turn-contents.ts');
const {checkedNativeContract}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {NativeContract}=await import('../../packages/contracts/src/native-contract.ts');
const {canonical}=await import('../../packages/contracts/src/canonical-json.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const catalog=JSON.parse(readFileSync('specs/native/codex-0.158.0/catalog.json','utf8'));
const selected=checkedNativeContract('0.158.0');
const clone=structuredClone, rows=[];
const itemParams=cursor=>({threadId:'thread',turnId:'turn',cursor,limit:16,sortDirection:'asc'});
function fixture(count=37) {
 const items=Array.from({length:count},(_,i)=>({id:'item-'+i,type:'agentMessage',text:'Text '+i+'\ud800'}));
 const observation=(id,method,params,reply)=>({observationId:id,nativeVersion:'0.158.0',catalogHash:selected.catalogHash,nativeExecutableHash:selected.catalog.nativeExecutableHash,epoch:'epoch',method,params,reply});
 const params={threadId:'thread',cursor:null,limit:1,sortDirection:'desc',itemsView:'notLoaded'};
 const snapshot=observation('snapshot','thread/turns/list',params,{result:{data:[{id:'turn',status:'completed',itemsView:'notLoaded',items:[]}],nextCursor:null}});
 const pages=[];
 for(let i=0;i<Math.max(count,1);i+=16)pages.push(observation('page-'+i,'thread/items/list',itemParams(i?String(i):null),{result:{data:items.slice(i,i+16).map(item=>({turnId:'turn',item})),nextCursor:i+16<count?String(i+16):null}}));
 const full=observation('full','thread/turns/list',{...params,itemsView:'full'},{result:{data:[{id:'turn',status:'completed',itemsView:'full',items}],nextCursor:null}});
 return {snapshot,pages,full,value:{format:'native-turn-item-pages-1',snapshot,pages}};
}
async function run(label,input) {
 const trace=[], pending=clone(input.responses??[]);
 function check(value,method,params,epoch,allowError=false){
  trace.push({kind:'check',method,params,epoch});
  requireThat(value.method===method&&canonical(value.params)===canonical(params)&&value.epoch===epoch,'native_evidence_mismatch','Test owner rejected changed query or epoch.');
  requireThat(allowError||'result' in value.reply,'native_read_failed','The native read returned its original error.');
 }
 const owner={checkRead:check,selectedContract:async()=>{trace.push({kind:'contract'});if(input.contractError)throw new IvyError('native_test_contract','Selected catalog unavailable.');return new NativeContract(catalog);},read:async(method,params,epoch,allowError=false)=>{
  trace.push({kind:'read',method,params,epoch,allowError});
  requireThat(pending.length>0,'native_test_exhausted','Unexpected owner read.');
  const observed=pending.shift();check(observed,method,params,epoch,allowError);return observed;
 }};
 let outcome;
 try {const value=input.kind==='read'?await readNativeTurnContents(owner,input.snapshot,'thread','turn',input.maximum):await verifyNativeTurnContents(owner,input.value,'thread','turn');outcome={ok:true,value};}
 catch(e){outcome={ok:false,error:IvyError.from(e).toWire()};}
 rows.push({label,input:JSON.stringify(input),expected:JSON.stringify({outcome,trace})});
}
for(const count of [0,1,16,17,32,37,65]){const f=fixture(count);await run('read-'+count,{kind:'read',snapshot:f.snapshot,responses:f.pages});await run('verify-'+count,{kind:'verify',value:f.value});await run('full-'+count,{kind:'verify',value:f.full});}
for(const maximum of [0,100,500,1000,2000,4000,8000,65536]){const f=fixture();await run('budget-'+maximum,{kind:'read',snapshot:f.snapshot,responses:f.pages,maximum});}
for(const code of [-32601,-32603,0])for(const offset of [0,1]){const f=fixture();f.pages[offset].reply={error:{code,message:'Original native error.'}};await run('error-'+code+'-'+offset,{kind:'read',snapshot:f.snapshot,responses:[...f.pages.slice(0,offset+1),f.full]});}
for(const status of ['failed','interrupted','inProgress','completed']){const f=fixture();f.pages[0].reply={error:{code:-32601,message:'Unsupported'}};f.full.reply.result.data[0].status=status;await run('fallback-status-'+status,{kind:'read',snapshot:f.snapshot,responses:[f.pages[0],f.full]});}
const mutations={
 empty:f=>f.value.pages=[],format:f=>f.value.format='other',pagesType:f=>f.value.pages={},
 observation:f=>f.pages[1].observationId=f.pages[0].observationId,snapshotObservation:f=>f.pages[0].observationId=f.snapshot.observationId,
 epoch:f=>f.pages[1].epoch='foreign',params:f=>f.pages[1].params.cursor='foreign',method:f=>f.pages[1].method='thread/turns/list',
 foreignTurn:f=>f.pages[1].reply.result.data[0].turnId='other',duplicate:f=>f.pages[1].reply.result.data[0].item.id='item-0',
 emptyId:f=>f.pages[0].reply.result.data[0].item.id='',numberId:f=>f.pages[0].reply.result.data[0].item.id=1,
 cursorRepeat:f=>f.pages[1].reply.result.nextCursor='16',cursorCycle:f=>f.pages[2].reply.result.nextCursor='16',
 emptyCursor:f=>f.pages[0].reply.result.nextCursor='',numberCursor:f=>f.pages[0].reply.result.nextCursor=1,missingCursor:f=>delete f.pages[0].reply.result.nextCursor,
 prematureEnd:f=>f.pages[0].reply.result.nextCursor=null,unfinished:f=>f.pages[2].reply.result.nextCursor='48',
 emptyIntermediate:f=>f.pages[0].reply.result.data=[],oversize:f=>f.pages[0].reply.result.data.push({turnId:'turn',item:{id:'extra',type:'agentMessage'}}),
 error:f=>f.pages[0].reply={error:{code:-32601,message:'Unsupported'}},
 limit:f=>f.snapshot.params.limit=2,query:f=>f.snapshot.params.itemsView='full',order:f=>f.snapshot.params.sortDirection='asc',
 turn:f=>f.snapshot.reply.result.data[0].id='other',turnCount:f=>f.snapshot.reply.result.data=[],
 status:f=>f.snapshot.reply.result.data[0].status='unknown',
 items:f=>f.snapshot.reply.result.data[0].items=[{}],view:f=>f.snapshot.reply.result.data[0].itemsView='full',
 fallbackCatalog:f=>{f.snapshot.catalogHash='sha256:'+'0'.repeat(64)},unsupportedVersion:f=>{f.snapshot.nativeVersion='unknown'},
};
for(const [name,mutate]of Object.entries(mutations)){const f=fixture();mutate(f);await run(name,{kind:'verify',value:f.value});}
for(const [name,mutate] of Object.entries(mutations).filter(([n])=>['cursorRepeat','cursorCycle','emptyCursor','numberCursor','missingCursor','prematureEnd','unfinished','emptyIntermediate','oversize','foreignTurn','duplicate','epoch','params'].includes(n))){const f=fixture();mutate(f);await run('read-'+name,{kind:'read',snapshot:f.snapshot,responses:f.pages});}
for(const fault of ['duplicate','empty','null','array','number']){const f=fixture();const items=f.full.reply.result.data[0].items;if(fault==='duplicate')items[1].id=items[0].id;else if(fault==='empty')items[0].id='';else items[0]=fault==='null'?null:fault==='array'?[]:1;await run('full-'+fault,{kind:'verify',value:f.full});}
{const f=fixture();f.snapshot.nativeVersion='unknown';await run('contract-error',{kind:'verify',value:f.value,contractError:true});}
process.stdout.write(JSON.stringify(rows));
