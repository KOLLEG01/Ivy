// Execute unchanged media.ts against its pinned wrapper/BufferJSON helpers,
// real files and the original SQLite journal. Network/codec/Main are explicit.
import {readFileSync,writeFileSync,existsSync,mkdirSync,mkdtempSync,readdirSync,statSync,utimesSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,relative,resolve} from 'node:path';
import {pathToFileURL,fileURLToPath} from 'node:url';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {createHash} from 'node:crypto';
const root=new URL('../../../',import.meta.url),url=p=>new URL(p,root).href;
const data=s=>'data:text/javascript;base64,'+Buffer.from(stripTypeScriptTypes(s,{mode:'transform'})).toString('base64');
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL?.startsWith('file:')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return{url:u.href,shortCircuit:true};}return n(s,c);},load(u,c,n){if(u.startsWith('file:')&&u.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(u),'utf8'),{mode:'transform'}),shortCircuit:true};return n(u,c);}});
const utils=process.env.IVY_CHAT_BAILEYS_UTILS_ROOT??fileURLToPath(new URL('node_modules/baileys/lib/Utils/',root));
const sources=['messages.js','generics.js'].map(n=>readFileSync(join(utils,n),'utf8'));
const expected=['3dde841c3caa0698862507e6c4268600494288817139f33152f490f3a715861f','5fba9fe6a000eb4a6e532904632877dbb98567f29143b9fcc974bd79d18d8468'];
const hash=b=>createHash('sha256').update(b).digest('hex');
sources.forEach((s,i)=>{if(hash(s)!==expected[i])throw Error('Pinned protocol source differs');});
const extract=(s,begin)=>s.slice(s.indexOf(begin),s.indexOf('\n};',s.indexOf(begin))+3);
const normal=extract(sources[0],'export const normalizeMessageContent ='),buffer=extract(sources[1],'export const BufferJSON =');
const sdk=data(`export {IvyError,requireThat} from '${url('packages/contracts/src/errors.ts')}';export {digest} from '${url('packages/contracts/src/canonical.ts')}';`);
const protocol=data(`${normal}\n${buffer}\nexport const decode=value=>JSON.parse(value,BufferJSON.reviver);export const downloadContentFromMessage=(...args)=>globalThis.waMediaProbe.download(...args);`);
const audio=data(`export const voiceWav=(bytes)=>globalThis.waMediaProbe.voice(bytes);`);
const scoped=data(`export const scopedOperationId=(client,scope)=>client.operationId(scope);`);
const journalSource=readFileSync(new URL('services/chat-bridge/src/whatsapp/journal.ts',root),'utf8').replaceAll("'../../../../packages/sdk/src/node.js'",JSON.stringify(sdk));
const {WhatsAppJournal}=await import(data(journalSource));
let source=readFileSync(new URL('services/chat-bridge/src/whatsapp/media.ts',root),'utf8');
for(const [name,replacement]of Object.entries({'baileys':protocol,'../../../../packages/sdk/src/node.js':sdk,'./auth.js':protocol,'./audio.js':audio,'../../../../packages/sdk/src/client.js':scoped,'./messages.js':url('services/chat-bridge/src/whatsapp/messages.ts')}))source=source.replaceAll(JSON.stringify(name),JSON.stringify(replacement));
const {mediaPayload,messageText,commandText,pruneMedia}=await import(data(source));
const {IvyError}=await import(url('packages/contracts/src/errors.ts')),{translator}=await import(url('services/chat-bridge/src/whatsapp/messages.ts'));
const specs=[],add=(label,patch={})=>specs.push({label,...patch}),settings={baseUrl:'https://speech.invalid/v1/',apiKey:'fixture-key',model:'fixture-model',language:'de'};
const image=(patch={})=>({imageMessage:{fileLength:3,mimetype:'image/png',...patch}}),audioMessage=(patch={})=>({audioMessage:{fileLength:3,seconds:1,...patch}}),document=(patch={})=>({documentMessage:{fileLength:3,fileName:'notes.txt',...patch}});
const checksum=createHash('sha256').update(Buffer.from([1,2,3])).digest();
add('text',{content:{conversation:'Hello'}});add('extended',{content:{extendedTextMessage:{text:'Extended'}}});
add('nullish-priority',{content:{conversation:null,extendedTextMessage:{text:'Selected'},imageMessage:{caption:'ignored'}}});
add('empty-conversation-selected',{content:{conversation:'',extendedTextMessage:{text:'Ignored'}}});
add('false-conversation-error',{content:{conversation:false}});add('null-message',{message:null});add('malformed-json',{encoded:'not JSON'});
for(const [label,content]of [['video',{videoMessage:{},conversation:'caption'}],['sticker',{stickerMessage:{},conversation:'caption'}],['empty',{}]])add(label,{content});
for(const [n,text]of [[65536,'a'],[65537,'a'],[65536,'😀'],[65537,'😀']])add('text-codepoints-'+n+'-'+text,{content:{conversation:text.repeat(n)}});
for(const key of ['ephemeralMessage','viewOnceMessage','documentWithCaptionMessage','viewOnceMessageV2','viewOnceMessageV2Extension','editedMessage','associatedChildMessage','groupStatusMessage','groupStatusMessageV2'])add('wrapper-'+key,{content:{[key]:{message:{conversation:'Wrapped'}}}});
let wrapped={conversation:'Five'};for(let i=0;i<5;i++)wrapped={ephemeralMessage:{message:wrapped}};add('wrapper-five',{content:wrapped});add('wrapper-six',{content:{editedMessage:{message:wrapped}}});
for(const saved of [{text:'Saved',images:[]},false,0,null])add('cached-payload-'+JSON.stringify(saved),{encoded:'not JSON',saved});
for(const sameHost of [true,false])for(const mimetype of ['image/png','image/jpeg','image/webp'])add('image-'+sameHost+'-'+mimetype,{content:image({mimetype}),sameHost});
add('image-remote-null-parent',{content:image(),sameHost:false,root:null});
add('image-caption',{content:image({caption:'Caption'})});add('image-german',{content:image(),language:'de'});add('image-types',{content:image({mimetype:'image/gif'})});
add('image-select-original',{content:image({url:null,directPath:'/original',thumbnailDirectPath:'/thumbnail'}),sameHost:false});
for(const fileLength of [null,0,-1,1.5,9007199254740992,'3','Infinity','NaN',{low:3,high:0}])add('declared-'+JSON.stringify(fileLength),{content:image({fileLength})});
add('declared-missing',{content:{imageMessage:{mimetype:'image/png'}}});
for(const [name,fileSha256]of [['base64',checksum.toString('base64')],['buffer',{type:'Buffer',data:checksum.toString('base64')}],['numeric',Object.fromEntries([...checksum].map((v,i)=>[i,v]))],['array',[...checksum]],['url',checksum.toString('base64url')],['wrong','AA==']])add('checksum-'+name,{content:image({fileSha256})});
add('length-mismatch',{content:image({fileLength:4})});add('empty-download',{content:image(),chunks:[]});
add('download-overflow',{content:image({fileLength:2097152}),sameHost:false,chunks:[{length:2097152,byte:7},{length:1,byte:8}]});
add('download-open-failure',{content:image(),fail:'download'});add('download-read-failure',{content:image(),fail:'next'});
add('cached-original',{content:image(),original:[1,2,3],fail:'download'});add('cached-original-empty',{content:image(),original:[]});add('cached-original-mismatch',{content:image(),original:[1,2]});
add('exclusive-original-collision',{content:image(),collision:true});
for(const fileLength of [undefined,null,0,-2,'NaN','Infinity','-Infinity',33554433])add('audio-length-'+String(fileLength),{content:audioMessage({fileLength}),config:{transcription:settings}});
add('audio-config-missing',{content:audioMessage()});add('audio-config-missing-cached',{content:audioMessage(),transcript:'cached'});
add('audio-unreliable-hash',{content:audioMessage({fileLength:1,fileSha256:'AA=='}),config:{transcription:settings}});
for(const seconds of [600,601,-2,'NaN','Infinity'])add('audio-seconds-'+String(seconds),{content:audioMessage({seconds}),config:{transcription:settings}});
for(const transcript of ['cached',''])add('cached-transcript-'+JSON.stringify(transcript),{content:audioMessage({seconds:601}),config:{transcription:settings},transcript});
add('audio-wav-large',{content:audioMessage(),config:{transcription:settings},wavLength:600*32000+4097});
add('audio-voice-error',{content:audioMessage(),config:{transcription:settings},fail:'voice'});
for(const [name,baseUrl]of [['http','http://speech.invalid/v1'],['invalid','not a URL'],['query','https://speech.invalid/v1/?query=yes'],['double-slash','https://speech.invalid/v1//']])add('transcription-url-'+name,{content:audioMessage(),config:{transcription:{...settings,baseUrl}}});
add('transcription-no-language',{content:audioMessage(),config:{transcription:{...settings,language:''}}});
add('transcription-model-error-before-url',{content:audioMessage(),config:{transcription:{...settings,model:{toString:1},baseUrl:'http://speech.invalid'}}});
add('transcription-base-missing',{content:audioMessage(),config:{transcription:{model:'fixture'}}});
for(const [name,response]of [['empty',{text:''}],['wrong',{text:42}],['null',null],['trim',{text:' \uFEFFHello\u00a0 '}],['limit',{text:'😀'.repeat(65000)}],['oversize',{text:'😀'.repeat(65001)}]])add('transcript-'+name,{content:audioMessage(),config:{transcription:settings},response});
for(const fail of ['fetch','json'])add('transcription-'+fail,{content:audioMessage(),config:{transcription:settings},fail});
add('transcription-rejected-before-json',{content:audioMessage(),config:{transcription:settings},ok:false,fail:'json'});
add('transcription-german',{content:audioMessage(),config:{transcription:settings},language:'de'});
for(const fileName of [undefined,'','folder/notes.txt','C:\\folder\\notes.txt','C:notes.txt','../strange\n👀\u0301\u2160é.txt','CON'])add('document-name-'+String(fileName),{content:document({fileName})});
add('document-remote',{content:document(),sameHost:false});add('document-german',{content:document(),language:'de'});add('document-caption',{content:document({caption:'Caption'})});
add('document-existing-same',{content:document(),documentFile:[1,2,3]});add('document-existing-changed',{content:document(),documentFile:[9]});
add('remote-main-error',{content:image(),sameHost:false,fail:'main'});add('remote-operation-error',{content:image(),sameHost:false,fail:'operationId'});add('remote-write-unknown',{content:image(),sameHost:false,fail:'write',outcome:'unknown'});
add('replayed-payload',{content:image(),sameHost:false,repeats:2});add('retry-transcription-after-error',{content:audioMessage(),config:{transcription:settings},failOnce:'fetch',repeats:2});
const bytes=chunk=>Array.isArray(chunk)?Buffer.from(chunk):Buffer.alloc(chunk.length,chunk.byte??0),capture=async f=>{try{return{value:await f()};}catch(e){const error=IvyError.from(e);return{error:{code:error.code,message:error.message,outcome:error.outcome}};}};
const temp=mkdtempSync(join(tmpdir(),'ivy-rust-wa-media-oracle-')),results=[];
try{
 for(let index=0;index<specs.length;index++){
  const spec=specs[index],directory=join(temp,String(index)),id='a'.repeat(64),mediaDirectory=join(directory,'whatsapp-media',id);mkdirSync(directory,{recursive:true});
  const journal=new WhatsAppJournal(':memory:'),calls=[],seen=new Map(),t=translator(spec.language),row={id,sender:'sender',jid:'fixture@invalid',message:spec.encoded??JSON.stringify({message:Object.hasOwn(spec,'message')?spec.message:spec.content})};
  const fail=name=>{const count=seen.get(name)??0;seen.set(name,count+1);if(spec.fail===name||(spec.failOnce===name&&count===0))throw new IvyError('fixture_'+name,'Original '+name,spec.outcome??'not_executed');};
  if(Object.hasOwn(spec,'saved'))journal.set('payload:'+id,spec.saved);if(Object.hasOwn(spec,'transcript'))journal.set('transcript:'+id,spec.transcript);
  if(Object.hasOwn(spec,'original')||spec.documentFile){mkdirSync(mediaDirectory,{recursive:true});if(Object.hasOwn(spec,'original'))writeFileSync(join(mediaDirectory,'original'),Buffer.from(spec.original));if(spec.documentFile)writeFileSync(join(mediaDirectory,'file-notes.txt'),Buffer.from(spec.documentFile));}
  globalThis.waMediaProbe={download:async(media,kind,options)=>{
   calls.push(['download',kind,options.options.signal.aborted,60000,...['url','directPath','thumbnailDirectPath'].map(k=>media[k]??null)]);fail('download');
   const parts=spec.chunks??[[1,2],[3]];if(spec.collision)writeFileSync(join(mediaDirectory,'original'),Buffer.from([9]));
   return{async *[Symbol.asyncIterator](){for(const part of parts){fail('next');yield bytes(part);}},destroy(){calls.push(['destroy']);}};
  },voice:async input=>{calls.push(['voice',input.length,hash(input)]);fail('voice');return Buffer.alloc(spec.wavLength??48,7);}};
  globalThis.fetch=async(input,options)=>{calls.push(['fetch',String(input),options.method,options.headers.Authorization,options.redirect,180000,[...options.body.keys()],options.body.get('model'),options.body.get('response_format'),options.body.get('language'),options.body.get('file').name,options.body.get('file').type,hash(Buffer.from(await options.body.get('file').arrayBuffer()))]);fail('fetch');return{ok:spec.ok??true,json:async()=>{calls.push(['json']);fail('json');return Object.hasOwn(spec,'response')?spec.response:{text:'  Spoken words  '};}};};
  const main={queue:{main:async()=>{calls.push(['main']);fail('main');return{value:{conversation:{objectId:'conversation'}}};}},store:{rootObjectId:Object.hasOwn(spec,'root')?spec.root:'root',client:{operationId:async scope=>{calls.push(['operationId',scope]);fail('operationId');return'original-image-operation';},request:async(method,request)=>{calls.push([method,request]);fail('write');return{object:{id:'image'},revision:{revision:7}};}}}};
  const texts=await capture(async()=>{const decoded=JSON.parse(row.message,(await import(protocol)).BufferJSON.reviver);return{message:messageText(decoded),command:commandText(decoded)};});
  const outcomes=[];for(let n=0;n<(spec.repeats??1);n++)outcomes.push(await capture(()=>mediaPayload(row,{main},journal,spec.config??{},directory,spec.sameHost??true,t)));
  const files=[];if(existsSync(mediaDirectory))for(const name of readdirSync(mediaDirectory).sort()){const b=readFileSync(join(mediaDirectory,name));files.push([name,b.length,hash(b)]);}
  const snapshot=Object.fromEntries(['kv','inbox','outgoing','retired_inbox','retired_outgoing'].map(table=>[table,journal.db.prepare('SELECT * FROM '+table+' ORDER BY '+(table==='kv'?'key':'id')).all().map(r=>Object.values(r))]));
  const normalize=v=>typeof v==='string'?v.replaceAll(directory.replaceAll('\\','\\\\'),'<root>').replaceAll(directory,'<root>'):Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)])):v;
  results.push({spec,row,texts,outcomes,calls,files,snapshot:normalize(snapshot)});await journal.close();
 }
 // Pruning touches only old lowercase-hex directories with no pending inbox.
 const directory=join(temp,'retention'),now=1700000000000,journal=new WhatsAppJournal(':memory:');mkdirSync(join(directory,'whatsapp-media'),{recursive:true});
 const names=['0'.repeat(64),'1'.repeat(64),'2'.repeat(64),'3'.repeat(64),'A'.repeat(64),'not-an-id'];
 for(const [i,name]of names.entries()){const path=join(directory,'whatsapp-media',name);mkdirSync(path);writeFileSync(join(path,'original'),Buffer.from([i]));utimesSync(path,new Date(now-8*86400000),new Date(now-(i===2?6:8)*86400000));journal.set('local-input:'+name,[{type:'image'}]);journal.set('payload:'+name,{text:'Retained payload',images:[]});}
 for(const id of [names[1],names[3]])journal.accept({id,sender:'sender',jid:'fixture@invalid',message:'{}',received:1});journal.finish(names[3]);
 const RealDate=Date;globalThis.Date=class extends RealDate{static now(){return now;}};
 const outcome=await capture(async()=>{await pruneMedia(directory,journal);return null;});globalThis.Date=RealDate;
 const retention={now,names,pending:[names[1]],complete:[names[3]],outcome,remaining:readdirSync(join(directory,'whatsapp-media')).sort(),kv:journal.db.prepare('SELECT * FROM kv ORDER BY key').all().map(r=>Object.values(r))};await journal.close();
 const corpus={version:1,dependency:{version:'7.0.0-rc13',files:expected},rows:results,retention};
 if(process.env.IVY_CHAT_WA_MEDIA_ORACLE_CAPTURE)writeFileSync(process.env.IVY_CHAT_WA_MEDIA_ORACLE_CAPTURE,JSON.stringify(corpus));process.stdout.write(JSON.stringify(corpus));
}finally{rmSync(temp,{recursive:true,force:true});}
