import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {createRequire,syncBuiltinESMExports} from 'node:module';
import {pathToFileURL} from 'node:url';
import {join} from 'node:path';
import {load,oracleRoot} from './inbox-fixtures.mjs';
const {canonical,hashJson,digest,IvyError}=await load('packages/sdk/src/node.js');
const require=createRequire(import.meta.url),threads=require('node:worker_threads'),OriginalWorker=threads.Worker,originalTimeout=globalThis.setTimeout;
const clone=structuredClone,rows=[],type={html:'text/html',docx:'application/vnd.openxmlformats-officedocument.wordprocessingml.document',pdf:'application/pdf'};
let selected,trace=[],currentFormat;
function document(reply){return{schemaVersion:1,format:reply.format??currentFormat,representation:'document_text',sections:[{label:'Projection',text:(reply.text??'Inert projection').repeat(reply.repeat??1)}],limitations:['visual_layout_not_interpreted','embedded_media_not_interpreted','external_resources_not_loaded'],...clone(reply.document??{})};}
function reply(value){if(value.kind==='code')return{code:value.code};if(value.kind==='raw')return clone(value.value);return{text:' '.repeat(value.padding??0)+canonical(document(value))};}
class Worker extends EventEmitter {
 constructor(url,options){super();const action=selected;this.stdout={resume(){}};this.stderr={resume(){}};
  currentFormat=Object.keys(type).find(key=>type[key]===options.workerData.mediaType);
  assert.deepEqual(options.resourceLimits,{maxOldGenerationSizeMb:128,maxYoungGenerationSizeMb:16,stackSizeMb:4});
  trace.push({format:currentFormat,budget:options.workerData.maximumInputBytes,byteLength:options.workerData.bytes.length,contentHash:digest(options.workerData.bytes)});
  queueMicrotask(()=>{if(action.reply.kind==='timeout')return;if(action.reply.kind==='error')this.emit('error',Error('Private diagnostic'));else if(action.reply.kind==='exit')this.emit('exit',1);else this.emit('message',reply(action.reply));});
 }
 terminate(){return Promise.resolve(1);}
}
threads.Worker=Worker;syncBuiltinESMExports();
globalThis.setTimeout=(callback,delay,...args)=>selected?.reply.kind==='timeout'&&delay===30000?originalTimeout(callback,0,...args):originalTimeout(callback,delay,...args);
function brief(value){return{kind:value.kind,source:clone(value.source),...(value.code?{code:value.code}:{}),contentHash:hashJson(value)};}
async function scenario(label,actions,options={}){
 trace=[];const input={label,actions:clone(actions),concurrent:options.concurrent??false};
 const {prepareAcquiredMediaInput}=await import(pathToFileURL(join(oracleRoot,'services/secretary/src/media-document.js')).href+'?scenario='+rows.length);
 const values=[];
 async function execute(action){selected=action;const bytes=Buffer.from(action.body??'owned').subarray(0),source={name:action.name??'inert',mediaType:action.mediaType??type[action.format??'html'],byteLength:action.byteLength??bytes.length,contentHash:action.contentHash??digest(bytes)};
  try{return{value:brief(await prepareAcquiredMediaInput(source,bytes,action.budget??6291456))};}catch(cause){const error=IvyError.from(cause);return{code:error.code,outcome:error.outcome};}
 }
 if(input.concurrent)values.push(...await Promise.all(actions.map(execute)));else for(const action of actions)values.push(await execute(action));
 rows.push({input,expected:{values,trace}});
}
const action=(body,reply={},extra={})=>({body,reply:{kind:'document',...reply},...extra});
try{
 await scenario('complete document and exact cache hit',[action('same'),action('same',{}, {name:'new original name'})]);
 await scenario('format and request budget distinguish cache entries',[action('same'),action('same',{}, {format:'docx'}),action('same',{}, {budget:1024}),action('same')]);
 for(const [label,extra]of [['zero budget',{budget:0}],['fractional budget',{budget:1.5}],['oversize budget',{budget:6291457}],['wrong length',{byteLength:3}],['wrong digest',{contentHash:'sha256:'+'f'.repeat(64)}]])await scenario(label,[action('valid',{},extra)]);
 await scenario('ECMAScript MIME normalization',[action('html',{}, {mediaType:'\ufeffTEXT/HTML ; charset=UTF-8\u00a0'}),action('html')]);
 await scenario('plain original text uses inert input',[action('safe',{}, {mediaType:'text/plain'})]);
 await scenario('unconfigured video preserves validation',[action('safe',{}, {mediaType:'video/mp4'}),action('safe',{}, {mediaType:'video/webm',budget:0})]);
 await scenario('selected request budget is exact',[action('safe',{}, {budget:1})]);
 for(const kind of ['error','exit','timeout'])await scenario('worker '+kind+' is not cached',[action('same',{kind}),action('same')]);
 for(const code of ['media_document_timeout','media_document_parser_failed','media_document_no_text','media_native_input_too_large','other','media_','media_BAD'])await scenario('worker error '+code,[action('same',{kind:'code',code}),action('same')]);
 for(const value of [null,[],{}, {text:'not JSON'}, {text:canonical({schemaVersion:1})}])await scenario('invalid worker reply '+JSON.stringify(value),[action('same',{kind:'raw',value}),action('same')]);
 await scenario('document format cannot change',[action('same',{format:'docx'}),action('same')]);
 await scenario('document schema cannot widen',[action('same',{document:{extra:true}}),action('same')]);
 await scenario('LRU promotion and seventeenth entry',[...Array.from({length:16},(_,n)=>action('entry-'+n)),action('entry-0'),action('entry-16'),action('entry-0'),action('entry-1')]);
 await scenario('cache byte cap evicts before count cap',[...Array.from({length:6},(_,n)=>action('large-'+n,{padding:4193000})),action('large-5',{padding:4193000}),action('large-0',{padding:4193000})]);
 await scenario('concurrent original misses both parse',[action('same'),action('same')],{concurrent:true});
 assert.equal(rows[0].expected.trace.length,1);assert.equal(rows.find(r=>r.input.label.startsWith('LRU promotion')).expected.trace.length,18);assert.equal(rows.find(r=>r.input.label.startsWith('cache byte cap')).expected.trace.length,7);
 console.log(JSON.stringify({rows}));
}finally{threads.Worker=OriginalWorker;syncBuiltinESMExports();globalThis.setTimeout=originalTimeout;}
