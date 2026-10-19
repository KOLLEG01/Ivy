import assert from 'node:assert/strict';
import {load,owner,rootId} from './inbox-fixtures.mjs';
import {outlookNativeCorpus as native} from './outlook-native-oracle.mjs';
const {OutlookNative,outlookName}=await load('services/secretary/src/outlook-native.js');
const {OutlookSource}=await load('services/secretary/src/outlook-source.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {IvyError,canonical,digest,hashJson}=await load('packages/sdk/src/node.js');
const clone=structuredClone,at=new Date(native.time).toISOString(),rows=[];
const originalNow=Date.now;Date.now=()=>native.wall;
const template=native.rows.find(row=>row.input.label==='original evidence reconstructs all calls').input;

async function add(label,options={}){
 const input=clone(template);input.label=label;input.mode=options.mode??'step';input.attempts=options.attempts??3;delete input.steps;
 input.documents=input.documents.filter(row=>row.key!=='secretary/outlook-evidence');
 const trace=[],steps=[],hive=new Map();let store,preparing=true,cut=false,failedWrite=false,owners=0,preparationEpochLookups=0;
 const source=input.core[0],plan=input.documents.find(row=>row.key==='secretary/outlook-plan');
 if(options.policy){Object.assign(input.settings.sources[0],options.policy);Object.assign(source.value.definition,options.policy);source.value.definitionHash=hashJson(source.value.definition);}
 const call=slot=>input.documents.find(row=>row.key==='secretary/outlook-call'&&row.value.slot===slot);
 const rewrite=(slot,edit)=>{const row=input.blobs.find(row=>row.id===call(slot).value.observation.chunks[0].objectId),operation=JSON.parse(Buffer.from(row.bytes,'base64'));edit(operation);const bytes=Buffer.from(canonical(operation));row.bytes=bytes.toString('base64');call(slot).value.observation={byteLength:bytes.length,contentHash:digest(bytes),chunks:[{objectId:row.id,revision:1}]};};
 if(options.messages!==undefined||options.more)rewrite(3,operation=>{const page=operation.reply.result.structuredContent,mail=clone(page.value[0]);page.value=Array.from({length:options.messages??1},(_,i)=>({...clone(mail),id:mail.id+'-'+i}));if(options.more)Object.assign(page,{has_more:true,next_from_index:13,next_link:'https://graph.microsoft.com/v1.0/me/messages?$skip=13'});});
 if(options.gap)rewrite(0,operation=>{operation.reply.result.account.email='foreign@example.test';});
 const raw=(value,id,name)=>({object:{id,parentId:rootId,name,effectivelyArchived:false,contractKey:'secretary/item'},revision:{revision:1,contractVersion:'1.3.0',mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:{},createdAt:at},content:{encoding:'json',value:clone(value)}});
 const client={async request(method,p,requestOptions){
  if(!preparing)trace.push({method,params:clone(p),timeout:requestOptions?.timeoutMs??35000});
  try{let value;
   if(!preparing&&++owners===options.failAt)throw new IvyError('fixture_rpc_failure','One original read reply was lost.','unknown');
   if(method==='serviceNodes.get')value={...owner,...(!preparing?options.owner:{})};
   else if(method==='system.status'){if(preparing)preparationEpochLookups++;value={runtimeEpoch:options.runtimeEpoch??'fixture-epoch'};}
   else if(method==='objects.query'){
    const name=p.where.args.find(value=>value.field==='object.name').value;
    value={items:[...hive.values()].filter(row=>row.object.name===name).map(row=>({objectId:row.object.id,revision:row.revision.revision})),nextCursor:null};
   }else if(method==='objects.read'){
    value=hive.get(p.objectId);if(!value||p.revision!==undefined&&p.revision!==value.revision.revision)throw new IvyError('not_found','Fixture object missing.');
   }else if(method==='objects.write'){
    const failure=!preparing&&!failedWrite&&options.writeFailure;failedWrite=true;
    if(failure&&!options.afterWrite)throw new IvyError('fixture_write_failure','One original item write reply was lost.','unknown');
    assert(p.create,'Only a new item is an external write in this workflow');
    const name=p.create.name,prior=[...hive.values()].find(row=>row.object.name===name);
    if(prior)throw new IvyError('mutation_conflict','Fixture name exists.');
    const id='00000000-0000-4000-8000-'+String(hive.size+100).padStart(12,'0');
    const saved=raw(p.content.value,id,name);saved.revision.references=clone(p.references);hive.set(id,saved);
    if(failure)throw new IvyError('fixture_write_failure','One original item write reply was lost.','unknown');
    value={object:{id},revision:{revision:1}};
   }else assert.fail('Unexpected source RPC '+method);
   if(!preparing)steps.push({method,value:clone(value)});return clone(value);
  }catch(cause){const e=IvyError.from(cause);if(!preparing)steps.push({method,error:{code:e.code,message:e.message,outcome:e.outcome,...(e.details===undefined?{}:{details:e.details})}});throw cause;}
 }};
 store=new SecretaryStore(client,rootId);
 for(const row of input.core){const value=canonical(row.value);store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,row.revision,0,null,value);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,row.revision,value);}
 for(const row of input.documents){const value=canonical(row.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(row.id,row.key,row.name,row.revision,value);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(row.id,row.revision,value);}
 for(const row of input.blobs)store.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run(row.id,row.key,row.name,Buffer.from(row.bytes,'base64'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:input.settings,epoch:Promise.resolve(options.runtimeEpoch??'fixture-epoch'),owner:{serviceNodeId:native.nodeId},signal:new AbortController().signal,now:()=>new Date(native.time)});
 const collector=new OutlookNative(engine,input.configuration),proof=await collector.collect(await collector.plan({objectId:'plan',revision:1}));assert(proof);
 let sourceWorker=new OutlookSource(engine,input.configuration);
 const originalAmend=store.technicalAmend.bind(store),originalLocalAmend=store.amend.bind(store);
 store.technicalAmend=(key,current,value)=>{
  if(preparing&&!cut&&options.cut===key&&(key!=='secretary/outlook-publication'||Boolean(value.checkpoint)===Boolean(options.checkpoint))){cut=true;throw new IvyError('fixture_persistence_cut','Preparation stopped before the original local amendment.','unknown');}
  return originalAmend(key,current,value);
 };
 store.amend=async(current,key,value)=>{
  if(preparing&&!cut&&options.cut==='receipt'&&key==='secretary/operation'&&value.phase==='succeeded'&&value.action===(options.checkpoint?'checkpoint':'capture')){cut=true;throw new IvyError('fixture_persistence_cut','Preparation stopped before the original local receipt.','unknown');}
  return originalLocalAmend(current,key,value);
 };
 const prepareResults=[];
 for(let n=0;n<(options.prefix??0);n++)try{prepareResults.push({value:await sourceWorker.step()});}catch(cause){const e=IvyError.from(cause);prepareResults.push({code:e.code,message:e.message,outcome:e.outcome});}
 if(options.cut)assert(cut,label+' must hit its declared local write boundary: '+JSON.stringify(prepareResults));
 input.preparation={results:prepareResults,cut:options.cut??null,checkpoint:options.checkpoint??false,epochLookups:preparationEpochLookups};
 function editLocal(table,id,edit){const row=store.db.prepare('SELECT current_revision,value_json FROM '+table+' WHERE id=?').get(id),value=JSON.parse(row.value_json);edit(value);const encoded=canonical(value);store.db.prepare('UPDATE '+table+' SET value_json=? WHERE id=?').run(encoded,id);store.db.prepare('UPDATE '+(table==='documents'?'revisions':'technical_revisions')+' SET value_json=? WHERE id=? AND revision=?').run(encoded,id,row.current_revision);}
 if(options.cursor)editLocal('documents','source',value=>{value.cursor='foreign-cursor';});
 const publication=store.db.prepare("SELECT id FROM technical_documents WHERE key='secretary/outlook-publication'").get();
 if(options.publication){assert(publication);editLocal('technical_documents',publication.id,options.publication);}
 if(options.publicationRevision){store.db.prepare('UPDATE technical_documents SET current_revision=? WHERE id=?').run(options.publicationRevision,publication.id);store.db.prepare('INSERT OR REPLACE INTO technical_revisions SELECT id,current_revision,value_json FROM technical_documents WHERE id=?').run(publication.id);}
 if(options.receipt){const receipt=store.db.prepare("SELECT id FROM documents WHERE key='secretary/operation' ORDER BY name LIMIT 1").get();assert(receipt);editLocal('documents',receipt.id,options.receipt);}
 if(options.head)editLocal('technical_documents','head',value=>options.head(value,proof.pin));
 const snapshot=()=>{
  const documents=store.db.prepare('SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all().map(row=>({id:row.id,key:row.key,name:row.name,revision:row.current_revision,value:JSON.parse(row.value_json)}));
  const revisions=store.db.prepare('SELECT id,revision,value_json FROM technical_revisions ORDER BY id,revision').all().map(row=>({id:row.id,revision:row.revision,value:JSON.parse(row.value_json)}));
  const core=store.db.prepare('SELECT id,key,name,current_revision,value_json,completed_at FROM documents ORDER BY key,name').all().map(row=>({id:row.id,key:row.key,name:row.name,revision:row.current_revision,value:JSON.parse(row.value_json),completed:row.completed_at!==null,history:store.db.prepare('SELECT revision,value_json FROM revisions WHERE id=? ORDER BY revision').all(row.id).map(r=>({revision:r.revision,value:JSON.parse(r.value_json)}))}));
  const acknowledgements=store.db.prepare('SELECT capture_id FROM capture_acknowledgements ORDER BY capture_id').all().map(row=>row.capture_id);
  return{documents,revisions,core,acknowledgements};
 };
 const initial=snapshot();Object.assign(input,initial);input.hive=clone([...hive.values()]);input.restart=options.restart??false;
 input.owner=options.owner??{};input.runtimeEpoch=options.runtimeEpoch??'fixture-epoch';
 sourceWorker=new OutlookSource(engine,input.configuration);preparing=false;const results=[];
 for(let attempt=0;attempt<input.attempts;attempt++){
  if(input.restart)sourceWorker=new OutlookSource(engine,input.configuration);
  try{results.push({value:input.mode==='head'?await sourceWorker.head():await sourceWorker.step()});}catch(cause){const e=IvyError.from(cause);results.push({code:e.code,message:e.message,outcome:e.outcome,...(e.details===undefined?{}:{details:e.details})});}
 }
 const retained=snapshot();retained.blobs=clone(input.blobs).sort((a,b)=>a.key.localeCompare(b.key,'en')||a.name.localeCompare(b.name,'en'));const ids=new Map([...retained.documents.map(row=>[row.id,'document:'+row.key+':'+row.name]),...retained.core.map(row=>[row.id,'core:'+row.key+':'+row.name]),...input.blobs.map(row=>[row.id,'blob:'+row.key+':'+row.name])]);
 const normalize=value=>typeof value==='string'?(ids.get(value)??value):Array.isArray(value)?value.map(normalize):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([key,value])=>[key,normalize(value)])):value;
 const expected=normalize({results,trace,retained,hive:[...hive.values()]});expected.retained.revisions.sort((a,b)=>a.id.localeCompare(b.id,'en')||a.revision-b.revision);expected.retained.acknowledgements.sort();
 input.steps=steps;rows.push({input,expected});store.close();
}
try{
 await add('one capture then checkpoint and cached idle',{attempts:4});
 await add('several captures each precede the single checkpoint',{messages:3,attempts:6});
 await add('empty page still requires checkpoint',{messages:0});
 await add('continuation is returned after original checkpoint',{more:true,attempts:2});
 await add('gap advances without capture or checkpoint',{gap:true});
 await add('ignored own sender remains an acknowledged capture',{policy:{ownSenderIds:['sender@example.test']}});
 await add('ignored sender policy remains an acknowledged capture',{policy:{allowedSenderIds:[]}});
 await add('accepted capture recovers its already written item',{prefix:1,cut:'receipt'});
 await add('capture receipt exists before publication acknowledgement',{prefix:1,cut:'secretary/outlook-publication'});
 await add('accepted checkpoint recovers its already committed cursor',{prefix:2,cut:'receipt',checkpoint:true});
 await add('checkpoint receipt exists before publication acknowledgement',{prefix:2,cut:'secretary/outlook-publication',checkpoint:true});
 await add('checkpoint publication exists before page head advancement',{prefix:2,cut:'secretary/outlook-head'});
 await add('restart after capture preserves original request IDs',{prefix:1,restart:true});
 await add('restart after complete checkpoint verifies last publication',{prefix:2,restart:true});
 await add('new checkpoint refuses foreign source cursor',{prefix:1,cursor:true});
 await add('new capture refuses foreign source cursor',{cursor:true});
 await add('uncertain item write before effect recovers original action',{writeFailure:true,attempts:4});
 await add('uncertain item write after effect recovers original item',{writeFailure:true,afterWrite:true,attempts:4});
 await add('new action refuses expired original runtime',{runtimeEpoch:'new-epoch'});
 await add('current owner is required before publication',{owner:{principalId:'other'}});
 for(const failAt of [1,3,6,10,15])await add('one original RPC failure '+failAt,{failAt,attempts:4});
 await add('publication evidence identity cannot change',{prefix:1,publication:value=>{value.evidence.objectId='other';}});
 await add('publication revision records every capture',{prefix:1,publicationRevision:4});
 await add('publication cannot exceed page captures',{prefix:1,publication:value=>{value.captured.push(clone(value.captured[0]));}});
 await add('capture receipt hash cannot change',{prefix:1,receipt:value=>{value.requestHash='sha256:'+'f'.repeat(64);}});
 await add('capture receipt phase cannot be relabelled',{prefix:1,receipt:value=>{value.phase='accepted';}});
 await add('completed head requires original publication',{head:(value,pin)=>{value.pending=null;value.lastPage=pin;},mode:'head',attempts:1});
 await add('completed head verifies original checkpoint',{prefix:2,mode:'head',attempts:2});
 for(const label of ['one capture then checkpoint and cached idle','several captures each precede the single checkpoint','accepted checkpoint recovers its already committed cursor','checkpoint receipt exists before publication acknowledgement','checkpoint publication exists before page head advancement'])assert(rows.find(row=>row.input.label===label).expected.results.some(result=>result.value==='complete'),label);
 assert(rows.find(row=>row.input.label==='gap advances without capture or checkpoint').expected.results.some(result=>result.value==='gap'));
 assert(rows.find(row=>row.input.label==='new capture refuses foreign source cursor').expected.results.every(result=>result.code==='microsoft_source_cursor_changed'));
 console.log(JSON.stringify({rootId,nodeId:native.nodeId,time:native.time,wall:native.wall,rows}));
}finally{Date.now=originalNow;}
