import './chat-domain-loader.mjs';
import {registerHooks} from 'node:module';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
const nativePlan=new URL('../../../services/chat-bridge/src/native-plan.ts',import.meta.url).href;
const probe=Symbol.for('ivy.test.chat-evidence-draft');
const wrapper='data:text/javascript;base64,'+Buffer.from(`import {validateChatNativeDraft as validate} from '${nativePlan}'; export function validateChatNativeDraft(value){return globalThis[Symbol.for('ivy.test.chat-evidence-draft')](value,validate);}`).toString('base64');
registerHooks({resolve(specifier,context,next){
 if(context.parentURL?.endsWith('/evidence-bytes.ts')&&specifier==='./native-plan.js')return{url:wrapper,shortCircuit:true};
 return next(specifier,context);
}});
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {saveChatEvidence,readChatEvidence}=await import('../../../services/chat-bridge/src/evidence-bytes.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {canonical,digest,hashJson}=await import('../../../packages/contracts/src/canonical.ts');
const {checkedNativeContract}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const now=1790000000123,at=new Date(now).toISOString(),RealDate=Date,chunkBytes=1048576,maximum=16*chunkBytes;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[now]));}static now(){return now;}};
const error=value=>{const e=IvyError.from(value);return{code:e.code,message:e.message,outcome:e.outcome};};
const summary=value=>{const encoded=canonical(value,32*chunkBytes);return{byteLength:Buffer.byteLength(encoded),contentHash:digest(encoded),...(encoded.length<512?{value}:{} )};};
const operation={schemaVersion:1,operationId:'original-operation',callerPrincipalId:'chat',serviceNodeId:'owner',nativeVersion:'0.154.0',nativeExecutableHash:digest('executable'),method:'thread/read',params:{threadId:'original-thread',text:'Original'},requestHash:'',createdAt:at,updatedAt:at,phase:'succeeded',epoch:'original-epoch',requestId:41,reply:{result:{original:true}},code:null};
operation.requestHash=hashJson({method:operation.method,params:operation.params});
const read={schemaVersion:1,observationId:'original-observation',callerPrincipalId:'chat',serviceNodeId:'owner',nativeVersion:'0.142.3',nativeExecutableHash:operation.nativeExecutableHash,catalogHash:digest('catalog'),method:operation.method,params:{threadId:'original-thread'},requestHash:hashJson({method:operation.method,params:{threadId:'original-thread'}}),epoch:'original-epoch',requestId:41,observedAt:at,reply:operation.reply};
const catalog=checkedNativeContract('0.154.0');
const draft={nativeVersion:'0.154.0',catalogSourceHash:catalog.catalog.sourceHash,method:'turn/interrupt',params:{threadId:'original-thread',turnId:'original-turn'}};
const formats={operation:'agent-operation/canonical-json',read:'agent-read-observation/canonical-json',draft:'native-request/canonical-json'};
function body(spec={}){
 const value=structuredClone(spec.kind==='read'?read:spec.kind==='draft'?draft:operation);
 if(spec.patch)Object.assign(value,structuredClone(spec.patch));
 if(spec.text!==undefined)value.params.text=spec.text;
 if(spec.splitUtf8){const encoded=canonical(value),prefix=Buffer.byteLength(encoded.slice(0,encoded.indexOf('"text":"')+8));value.params.text='x'.repeat(chunkBytes-1-prefix)+'😀end';}
 if(spec.byteLength){value.params.text='';value.params.text='x'.repeat(spec.byteLength-Buffer.byteLength(canonical(value)));}
 if('requestHash'in value)value.requestHash=hashJson({method:value.method,params:value.params});
 return value;
}
const publicInput={object:{id:'original-input',parentId:'root',name:'Input',contractKey:'chat-bridge/input',effectivelyArchived:false},revision:{revision:1,contractVersion:'1.3.0',mediaType:'application/json'},content:{encoding:'json',value:{workspaceId:'chat'}}};
const baseRoot=mkdtempSync(join(tmpdir(),'ivy-chat-evidence-seed-'));
const seed=new ChatStore({async request(){throw Error('Unexpected seed RPC');}},'root',baseRoot);
const parent=await seed.write('chat-bridge/operation',{operationId:`chat:${now}:parent`,phase:'accepted',updatedAt:at},'seed',{create:{parentId:'root',name:'Original parent'}});
seed.close();
{const db=new DatabaseSync(join(baseRoot,'chat-bridge.sqlite'));db.exec(`CREATE TABLE evidence_test_writes(value_json TEXT); CREATE TRIGGER evidence_test_revision AFTER INSERT ON revisions WHEN EXISTS(SELECT 1 FROM documents WHERE id=NEW.id AND key='chat-bridge/evidence') BEGIN INSERT INTO evidence_test_writes VALUES(NEW.value_json); END;`);db.close();}
const database=readFileSync(join(baseRoot,'chat-bridge.sqlite')).toString('base64');rmSync(baseRoot,{recursive:true,force:true});
const save=(spec={},extra={})=>({action:'save',name:'evidence',body:spec,format:formats[spec.kind??'operation'],...extra});
const load=(extra={})=>({action:'read',name:'evidence',format:formats.operation,...extra});
const reopen={action:'reopen'};
const cases=[],add=(label,steps,extra={})=>cases.push({label,steps,...extra});
add('operation-idempotent-reopen',[save(),load(),save(),reopen,load()]);
add('original-read-observation',[save({kind:'read'}),load({format:formats.read}),reopen,load({format:formats.read})]);
add('native-draft-required-gate',[save({kind:'draft'}),load({format:formats.draft})]);
add('invalid-native-draft-before-scope',[save({kind:'draft',patch:{params:{threadId:4}}},{parent:'missing'})]);
add('invalid-operation-before-scope',[save({patch:{phase:'invalid'}},{parent:'missing'})]);
add('invalid-read-before-scope',[save({kind:'read',patch:{observedAt:null}},{parent:'missing'})]);
add('unknown-format-before-scope',[save({}, {format:'other/canonical-json',parent:'missing'})]);
add('remote-input-parent-scope',[save({}, {parent:'input'}),load({parent:'input'}),reopen,load({parent:'input'})]);
for(const [label,patch]of [['wrong-root',{object:{parentId:'other'}}],['archived',{object:{effectivelyArchived:true}}],['wrong-contract',{object:{contractKey:'other'}}],['wrong-version',{revision:{contractVersion:'2.0.0'}}]])add('parent-'+label,[{action:'remote-patch',patch},save({}, {parent:'input'})]);
add('parent-owner-outage-is-not-scope-fallback',[{action:'remote-error',error:{code:'original_outage',message:'Original parent read unavailable.',outcome:'unknown'}},save({}, {parent:'input'})]);
add('unknown-parent',[save({}, {parent:'missing'})]);
add('wrong-parent-on-read',[save(),load({parent:'input'})]);
add('wrong-format-on-read',[save(),load({format:formats.read})]);
add('wrong-manifest-pin',[save(),load({revision:7})]);
for(const [label,patch]of [['format',{format:formats.read}],['count',{byteLength:2*chunkBytes}],['body-hash',{contentHash:digest('wrong')}],['missing-chunks',{chunks:null}]])add('manifest-'+label,[save(),{action:'patch-manifest',patch},load()]);
for(const [label,patch]of [['media',{mediaType:'text/plain'}],['length',{byteLength:1}],['hash',{contentHash:digest('wrong')}],['revision',{object:{revision:2}}],['missing',{object:{objectId:'missing'}}]])add('chunk-'+label,[save(),{action:'patch-chunk',patch},load()]);
add('changed-chunk-bytes',[save(),{action:'blob-edit',mode:'flip'},load()]);
add('missing-chunk-bytes',[save(),{action:'blob-edit',mode:'delete'},load()]);
add('foreign-chunk-parent',[save(),{action:'blob-edit',mode:'parent'},load()]);
add('ordered-chunks-and-full-body-hash',[save({byteLength:chunkBytes+800}),{action:'reverse-chunks'},load()]);
add('duplicate-chunk-pins',[save({byteLength:chunkBytes+800}),{action:'duplicate-chunk'},load()]);
add('whole-utf8-joined-across-chunks',[save({splitUtf8:true}),load(),reopen,load()]);
add('unpaired-utf16-canonical-body',[save({text:'x\ud800y\udfff😀'}),load()]);
for(const [label,mode]of [['whitespace','whitespace'],['duplicate-property','duplicate'],['invalid-json','json'],['invalid-utf8','utf8'],['bom','bom'],['wrong-body-shape','shape']])add('body-'+label,[save(),{action:'replace-body',mode},load()]);
add('blob-capacity-rollback',[{action:'limits',count:1},save(),{action:'limits',count:100},save(),load()]);
add('manifest-capacity-after-committed-chunks',[{action:'limits',count:3},save({byteLength:chunkBytes+800}),reopen,{action:'limits',count:100},save({byteLength:chunkBytes+800}),load()]);
for(const length of [chunkBytes-1,chunkBytes,chunkBytes+1,maximum,maximum+1])add('actual-body-bytes-'+length,[save({byteLength:length}),...(length<=maximum?[load()]:[])],{group:'limits'});
for(const phase of ['accepted','dispatched','outcome_unknown','failed'])add('operation-shape-'+phase,[save({patch:{phase,...(phase==='accepted'?{epoch:null,requestId:null,reply:null}:phase==='outcome_unknown'?{reply:null,code:'outcome_unknown'}:phase==='dispatched'?{reply:null}:{code:'native_error',reply:{error:{code:-1,message:'Original failure'}}})}}),load()]);
const snapshots=[];
for(const c of cases){
 const root=mkdtempSync(join(tmpdir(),'ivy-chat-evidence-')),file=join(root,'chat-bridge.sqlite');writeFileSync(file,Buffer.from(c.database??database,'base64'));
 const calls=[],draftCalls=[],pins=new Map(Object.entries(c.initialPins??{})),aliases=new Map();let remote=structuredClone(publicInput),remoteError=null;
 globalThis[probe]=(value,validate)=>{const call={input:summary(value)};draftCalls.push(call);try{validate(value);call.outcome={value:null};}catch(e){call.outcome={error:error(e)};throw e;}};
 const client={async request(method,params){calls.push({method,params:structuredClone(params)});if(remoteError)throw new IvyError(remoteError.code,remoteError.message,remoteError.outcome);if(method!=='objects.read')throw Error('Unexpected RPC '+method);if(params.objectId!=='original-input')throw new IvyError('object_missing','Original parent is unavailable.');return structuredClone(remote);}};
 let store=new ChatStore(client,'root',root),last=0;c.expected=[];
 const parentFor=step=>step.parent==='input'?'original-input':step.parent==='missing'?'missing':parent.objectId;
 const saved=()=>{const db=new DatabaseSync(file),row=db.prepare('SELECT value_json FROM revisions WHERE id=? ORDER BY revision DESC LIMIT 1').get(pins.get('evidence')?.objectId);db.close();return JSON.parse(row.value_json);};
 const patchManifest=value=>{const db=new DatabaseSync(file);db.prepare('UPDATE revisions SET value_json=? WHERE id=? AND revision=(SELECT current_revision FROM documents WHERE id=?)').run(JSON.stringify(value),pins.get('evidence').objectId,pins.get('evidence').objectId);db.close();};
 const snapshot=output=>{
  const db=new DatabaseSync(file),docs=db.prepare("SELECT d.id,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/evidence' ORDER BY d.name").all(),blobs=db.prepare('SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,parent_id').all();
  for(const r of [...docs,...blobs])aliases.set(r.id,'<local:'+r.name+'>');
  const normalize=value=>typeof value==='string'?aliases.get(value)??value:Array.isArray(value)?value.map(normalize):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v)])):value;
  const writes=db.prepare('SELECT rowid,value_json FROM evidence_test_writes WHERE rowid>? ORDER BY rowid').all(last);last=writes.at(-1)?.rowid??last;db.close();
  const answer=normalize({output,documents:docs.map(r=>({id:r.id,name:r.name,parent:r.parent_id,revision:r.current_revision,value:JSON.parse(r.value_json)})),blobs:blobs.map(r=>({id:r.id,key:r.key,name:r.name,parent:r.parent_id,byteLength:r.bytes.length,contentHash:digest(r.bytes),...(r.bytes.length<64?{base64:Buffer.from(r.bytes).toString('base64')}:{})})),writes:writes.map(r=>JSON.parse(r.value_json)),calls,draftCalls,changeVersion:store.changeVersion});calls.length=0;draftCalls.length=0;return answer;
 };
 try{
  for(const step of c.steps){let output;try{let value=null;
   switch(step.action){
    case 'save':value=await saveChatEvidence(store,parentFor(step),step.format,body(step.body));pins.set(step.name,value);break;
    case 'read':value=summary(await readChatEvidence(store,parentFor(step),{...pins.get(step.name),...(step.revision?{revision:step.revision}:{})},step.format));break;
    case 'reopen':store.close();store=new ChatStore(client,'root',root);break;
    case 'limits':store.limits.count=step.count;break;
    case 'remote-error':remoteError=step.error;break;
    case 'remote-patch':for(const [key,patch]of Object.entries(step.patch))Object.assign(remote[key],patch);break;
    case 'patch-manifest':patchManifest({...saved(),...step.patch});break;
    case 'patch-chunk':{const manifest=saved(),first=manifest.chunks[0];for(const [key,v]of Object.entries(step.patch))first[key]=key==='object'?{...first[key],...v}:v;patchManifest(manifest);break;}
    case 'reverse-chunks':{const manifest=saved();manifest.chunks.reverse();patchManifest(manifest);break;}
    case 'duplicate-chunk':{const manifest=saved();manifest.chunks[1]=structuredClone(manifest.chunks[0]);patchManifest(manifest);break;}
    case 'blob-edit':{const id=saved().chunks[0].object.objectId,db=new DatabaseSync(file);if(step.mode==='delete')db.prepare('DELETE FROM blobs WHERE id=?').run(id);else if(step.mode==='parent')db.prepare('UPDATE blobs SET parent_id=? WHERE id=?').run('foreign',id);else{const bytes=Buffer.from(db.prepare('SELECT bytes FROM blobs WHERE id=?').get(id).bytes);bytes[0]^=1;db.prepare('UPDATE blobs SET bytes=? WHERE id=?').run(bytes,id);}db.close();break;}
    case 'replace-body':{const encoded=canonical(body()),bytes=step.mode==='utf8'?Buffer.from([255]):Buffer.from(step.mode==='whitespace'?' '+encoded:step.mode==='duplicate'?encoded.replace('{','{"schemaVersion":1,'):step.mode==='json'?'not JSON':step.mode==='shape'?'{}':'\ufeff'+encoded),manifest=saved(),hash=digest(bytes);manifest.byteLength=bytes.length;manifest.contentHash=hash;manifest.chunks=[];for(let offset=0,i=0;offset<bytes.length;offset+=chunkBytes,i++){const part=bytes.subarray(offset,offset+chunkBytes),object=store.saveLocalBlob('chat-bridge/evidence-chunk','Replaced evidence '+hash.slice(7)+' part '+i,parent.objectId,part);manifest.chunks.push({object,contentHash:digest(part),byteLength:part.length,mediaType:'application/octet-stream',label:'Native evidence part '+(i+1)});}patchManifest(manifest);break;}
    default:throw Error('Unknown fixture action '+step.action);
   }output={value};
  }catch(e){output={error:error(e)};}c.expected.push(snapshot(output));}
  if(c.label==='manifest-capacity-after-committed-chunks'){
   if(c.expected[1].output.error?.code!=='capacity_exceeded'||c.expected[1].blobs.length!==2||c.expected[1].documents.length)throw Error('Partial chunk commit was not exercised');
  }
  if(c.label==='whole-utf8-joined-across-chunks'){
   const db=new DatabaseSync(file),bytes=Buffer.from(db.prepare('SELECT bytes FROM blobs ORDER BY name LIMIT 1').get().bytes);db.close();if(bytes.at(-1)!==240)throw Error('UTF8 scalar did not cross the real chunk boundary');
  }
  if(c.label==='body-bom'&&c.expected.at(-1).output.error)throw Error('Original TextDecoder BOM behavior was not exercised');
  if(c.label==='native-draft-required-gate'&&c.expected.some(s=>s.output.error||s.draftCalls.length!==1))throw Error('Original native draft validation was not exercised: '+JSON.stringify(c.expected.map(s=>s.output)));
  if(c.label==='original-read-observation'){if(!pins.has('evidence')||c.expected.some(s=>s.output.error))throw Error('Original read fixture failed: '+JSON.stringify(c.expected.map(s=>s.output)));store.close();snapshots.push({label:'original-sqlite-read-reconstruction',database:readFileSync(file).toString('base64'),initialPins:Object.fromEntries(pins),steps:[load({format:formats.read})]});store=new ChatStore(client,'root',root);}
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
}
// Preserve a source-created complete database and read it without generating new pins.
for(const c of snapshots){
 const root=mkdtempSync(join(tmpdir(),'ivy-chat-evidence-copy-'));writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(c.database,'base64'));const store=new ChatStore({async request(){throw Error('Unexpected copied RPC');}},'root',root);
 const value=await readChatEvidence(store,parent.objectId,c.initialPins.evidence,formats.read);c.expectedBody=summary(value);store.close();rmSync(root,{recursive:true,force:true});
}
process.stdout.write(JSON.stringify({now,parent,database,bodies:{operation,read,draft},cases,snapshots,nativeDraftBoundary:'The actual original native validator is observed here. Rust tests replay its outcomes to prove the required gate/order only; Rust native invocation validation remains an owner dependency.'}));
