import './chat-store-loader.mjs';
import {registerHooks} from 'node:module';
const exported={
 '../../../packages/contracts/src/canonical.ts':['canonical','digest','hashJson'],
 '../../../packages/contracts/src/errors.ts':['IvyError','requireThat'],
 '../../../packages/contracts/src/chat-validation.ts':['validateChat'],
 '../../../packages/contracts/src/agent-validation.ts':['validateAgent'],
 '../../../packages/contracts/src/checked-native-contract.ts':['checkedNativeContract'],
 '../../../packages/contracts/src/operation-id.ts':['parseOperationId'],
 '../../../packages/contracts/src/bundle.ts':['bundledSchema'],
 '../../../packages/sdk/src/receipt-archive.ts':['hasUnresolvedOutcome'],
};
const sdk='data:text/javascript;base64,'+Buffer.from(Object.entries(exported).map(([path,names])=>`export {${names.join(',')}} from '${new URL(path,import.meta.url)}';`).join('\n')).toString('base64');
registerHooks({resolve(specifier,context,next){
 if(specifier==='../../../packages/sdk/src/node.js')return{url:sdk,shortCircuit:true};
 return next(specifier,context);
}});
const {readChatResultText,chatReplyParts}=await import('../../../services/chat-bridge/src/result-text.ts');
const {digest}=await import('../../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const result={schemaVersion:1,inputId:'input',binding:{objectId:'binding',revision:1},turnId:'original-turn',nativeState:'completed',evidence:[{objectId:'evidence',revision:1}],textParts:[],createdAt:'2026-10-18T00:00:00.000Z'};
const outcome=async fn=>{try{return{value:await fn()};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}};
const artifact=(id,text)=>({object:{objectId:id,revision:1},contentHash:digest(Buffer.from(text)),byteLength:Buffer.byteLength(text),mediaType:'text/plain',label:'Original '+id});
const record=(id,text)=>({object:{id,parentId:'input',contractKey:'chat-bridge/result-text',effectivelyArchived:false},revision:{revision:1,contractVersion:'1.0.0',mediaType:'text/plain',contentHash:digest(Buffer.from(text)),byteLength:Buffer.byteLength(text)},content:{encoding:'text',value:text}});
const reads=[];
const add=(label,texts,edit=()=>{})=>{
 const row={label,inputId:'input',result:{...structuredClone(result),textParts:texts.map((text,i)=>artifact('part-'+i,text))},records:texts.map((text,i)=>({value:record('part-'+i,text)}))};
 edit(row);reads.push(row);return row;
};
add('empty-result',[]);
add('original-unicode-bytes',['Original\n\r\0😀é漢字\ufffd']);
add('parts-join-without-inserted-separators',['one\n','\n😀two','last']);
add('many-distinct-immutable-parts',Array.from({length:65},(_,i)=>'Part '+i+'\n'));
add('exact-one-mib-part',['x'.repeat(1024*1024)],row=>{
 row.records[0].repeat={unit:'x',count:1024*1024};row.records[0].value.content.value='';
});
add('wrong-input-before-read',['text'],row=>row.inputId='another-input');
add('invalid-result-before-read',['text'],row=>delete row.result.binding);
add('part-revision-before-read',['text'],row=>row.result.textParts[0].object.revision=2);
add('part-media-before-read',['text'],row=>row.result.textParts[0].mediaType='application/octet-stream');
add('oversize-part-before-read',['text'],row=>row.result.textParts[0].byteLength=1024*1024+1);
add('duplicate-part-after-first-read',['one','two'],row=>row.result.textParts[1]=structuredClone(row.result.textParts[0]));
for(const [label,path,value]of [
 ['id',['object','id'],'different'],['parent',['object','parentId'],'another-input'],
 ['archived',['object','effectivelyArchived'],true],['key',['object','contractKey'],'other-contract'],
 ['revision',['revision','revision'],2],['version',['revision','contractVersion'],'1.1.0'],
 ['media',['revision','mediaType'],'application/octet-stream'],['encoding',['content','encoding'],'base64'],
 ['length',['revision','byteLength'],999],['hash',['revision','contentHash'],digest('different')],
 ['rewritten-text',['content','value'],'different'],['unpaired-high',['content','value'],'\ud800'],
 ['unpaired-low',['content','value'],'\udfff'],
])add('retained-'+label,['Original'],row=>row.records[0].value[path[0]][path[1]]=value);
add('declared-content-hash',['Original'],row=>row.result.textParts[0].contentHash=digest('changed'));
add('declared-byte-length',['Original'],row=>row.result.textParts[0].byteLength++);
add('missing-original-revision',['Original'],row=>row.records=[]);
add('first-read-outage',['one'],row=>row.failureAt=1);
add('second-read-outage',['one','two'],row=>row.failureAt=2);
{
 const text='x'.repeat(1024*1024),pin=artifact('part-0',text),saved=record('part-0',text);
 saved.content.value='';
 const row=add('complete-total-byte-bound',[]);
 row.result.textParts=Array.from({length:65},(_,i)=>({...pin,object:{objectId:'total-'+i,revision:1}}));
 row.records=row.result.textParts.map(part=>({value:{...structuredClone(saved),object:{...saved.object,id:part.object.objectId}},repeat:{unit:'x',count:1024*1024}}));
}
for(const row of reads){
 const calls=[];
 const store={client:{async request(method,params){
  calls.push({method,params:structuredClone(params)});
  if(row.failureAt===calls.length)throw new IvyError('fixture_outage','Original text read unavailable.','unknown');
  const spec=row.records.find(r=>r.value.object.id===params.objectId);
  if(!spec)throw new IvyError('revision_pruned','Original text revision is unavailable.');
  const value=structuredClone(spec.value);
  if(spec.repeat)value.content.value=spec.repeat.unit.repeat(spec.repeat.count);
  return value;
 }}};
 row.expected=await outcome(()=>readChatResultText(store,row.inputId,row.result));row.calls=calls;
 if(row.label==='complete-total-byte-bound'&&(row.expected.error?.code!=='chat_collection_limit'||calls.length!==64))throw Error('Total text bound was not exercised.');
 if(row.label==='parts-join-without-inserted-separators'&&row.expected.value!=='one\n\n😀twolast')throw Error('Complete text concatenation was not exercised.');
}
const presentations=[];
const sampleArtifacts=Array.from({length:65},(_,i)=>artifact('presentation-'+i,'x'));
const present=(label,text,count=0,state='completed',edit=()=>{})=>{
 const input={...structuredClone(result),nativeState:state,textParts:structuredClone(sampleArtifacts.slice(0,count))};edit(input);
 presentations.push({label,text,result:input});
};
for(const length of [0,1,16383,16384,16385,32768,32769]){
 present('ascii-'+length,'a'.repeat(length));
 present('astral-'+length,'😀'.repeat(length));
}
for(const [label,text]of [
 ['mixed-boundary','x'.repeat(16383)+'😀last'],
 ['high-boundary','x'.repeat(16383)+'\ud800last'],
 ['low-boundary','x'.repeat(16383)+'\udfff😀last'],
 ['surrogate-context','\udfff\ud800x\ud800\udfff\ud800\ud800\udfff\u0000'],
 ['line-separators','\n\r\u2028\u2029\t'],
])present(label,text,65);
for(const count of [1,31,32,33,64,65])for(const text of ['','short','😀'.repeat(16385)+'last'])present('artifact-pages-'+count+'-'+text.length,text,count);
for(const state of ['completed','failed','interrupted',null,4,{},['native','state'],'\ud800'])present('empty-state-'+JSON.stringify(state),'',0,state);
present('missing-state','',0,'completed',result=>delete result.nativeState);
present('missing-artifacts','body',0,'completed',result=>delete result.textParts);
for(const row of presentations){
 row.expected=await outcome(()=>chatReplyParts(row.result,row.text));
 if(row.expected.value){
  if(row.text&&row.expected.value.map(part=>part.text).join('')!==row.text)throw Error('Presentation lost original text: '+row.label);
  if(JSON.stringify(row.expected.value.flatMap(part=>part.artifacts))!==JSON.stringify(row.result.textParts))throw Error('Presentation lost artifact order: '+row.label);
  if(row.expected.value.some(part=>[...part.text].length>16384||part.artifacts.length>32))throw Error('Presentation exceeded a part bound.');
 }
}
process.stdout.write(JSON.stringify({reads,presentations}));
