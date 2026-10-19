import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,readFile,readdir,realpath,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname,basename} from 'node:path';
import {load} from './inbox-fixtures.mjs';
const {VideoPreparer}=await load('services/secretary/src/media-video.js');
const {validateVideo}=await load('services/secretary/src/video-schema.js');
const {canonical,digest,hashJson,IvyError}=await load('packages/sdk/src/node.js');
const rows=[],clone=structuredClone;
const error=cause=>{const e=IvyError.from(cause);return{code:e.code,outcome:e.outcome};};
const normalize=(value,root)=>typeof value==='string'?value.replaceAll(root,'<root>'):Array.isArray(value)?value.map(v=>normalize(v,root)):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v,root)])):value;
function prepared(value,compact){if(compact&&value.kind==='video')return{kind:value.kind,source:value.source,inputsHash:hashJson(value.inputs),inputBytes:Buffer.byteLength(canonical(value.inputs)),projection:JSON.parse(value.inputs[0].text)};return clone(value);}
function source(options,id=0){const bytes=Buffer.alloc(options.length??16);if(bytes.length>=16){if(options.type==='video/x-msvideo'){bytes.write('RIFF');bytes.write('AVI ',8);}else if(['video/webm','video/x-matroska'].includes(options.type)){bytes.set([0x1a,0x45,0xdf,0xa3]);}else bytes.write('ftyp',4);bytes[15]=id;}if(options.badSignature)bytes[4]=0;if(options.highAscii)for(const index of (options.type==='video/x-msvideo'?[0,1,2,3,8,9,10,11]:[4,5,6,7]))bytes[index]|=128;
 return{bytes,value:{name:options.name??'../../outside; $(inert).mp4',mediaType:options.type??'video/mp4',byteLength:bytes.length+(options.wrongLength?1:0),contentHash:options.wrongHash?digest('other'):digest(bytes)}};}
async function add(label,options={}){
 const root=await mkdtemp(join(tmpdir(),'secretary-video-reference-')),parent=await realpath(tmpdir()),work=join(root,'work'),trace=[],results=[];
 try{
  const decoder=join(root,'decoder.exe'),probePath=join(root,'probe.exe');await writeFile(decoder,'decoder');await writeFile(probePath,'probe');
  const configuration={schemaVersion:1,ffmpeg:{path:decoder,hash:digest(options.changedDecoder?'other':'decoder')},ffprobe:{path:probePath,hash:digest('probe')}};
  if(options.scratch){await mkdir(work);for(let n=0;n<(options.scratch.entries??1);n++){
   const dir=join(work,options.scratch.badName?'foreign-'+n:'video-retained-'+n);if(options.scratch.entryFile){await writeFile(dir,'x');continue;}await mkdir(dir);
   for(let k=0;k<(options.scratch.files??0);k++){const path=join(dir,'file-'+k);await writeFile(path,'x');if(options.scratch.large){const file=await import('node:fs/promises').then(fs=>fs.open(path,'r+'));await file.truncate(options.scratch.large);await file.close();}}
  }}
  const worker=new VideoPreparer(work,configuration);
  let commandIndex=0;
  worker.command=async(path,args,cwd,maximumBytes,timeoutMs)=>{
   commandIndex++;trace.push({executable:path,args:args.map(arg=>arg.replaceAll(cwd,'<scratch>')),cwd:'<scratch>',maximumBytes,timeoutMs});await Promise.resolve();
   if(options.failAt===commandIndex){if(options.uncoded)throw Error('ordinary decoder failure');throw new IvyError(options.failure??'media_video_timeout','The bounded local video decoder did not complete.');}
   if(args.includes('-show_entries')){
    if(options.invalidJson)return Buffer.from('{broken');if(options.bomJson)return Buffer.from('\ufeff{}');
    return Buffer.from(JSON.stringify(options.probe??{format:{duration:2,start_time:'0'},streams:[{index:0,codec_type:'video',width:320,height:240},...(options.silent?[]:[{index:1,codec_type:'audio',duration:0.02}])]}));
   }
   if(args.includes('image2')){
    for(let n=0;n<(options.frames??8);n++){const frame=Buffer.alloc(options.frameSize??4);frame[0]=255;frame[1]=216;frame[frame.length-2]=255;frame[frame.length-1]=options.badFrame?0:217;await writeFile(join(cwd,'frame-'+String(n+1+(options.frameGap&&n>0?1:0)).padStart(2,'0')+'.jpg'),frame);}
    return Buffer.alloc(0);
   }
   assert(args.includes('s16le'));return Buffer.alloc(options.pcmSize??640,options.pcmByte??255);
  };
  const actions=options.actions??[{id:0,budget:5*1024*1024}];
  for(const action of actions){const input=source(options,action.id);if(action.name)input.value.name=action.name;
   try{const value=await worker.prepare(input.value,input.bytes,action.budget);results.push({value:prepared(value,options.compact)});if(action.mutate&&value.kind==='video')value.inputs[0].text='substituted';}catch(cause){results.push(error(cause));}
  }
  await worker.close();const retained=(await readdir(root)).sort(),scratch=(retained.includes('work')?await readdir(work):[]).sort();
  rows.push({input:{label,options,actions,configuration:normalize(configuration,root)},expected:normalize({results,trace,retained,scratch},root)});
 }finally{const actual=await realpath(root);assert.equal(dirname(actual),parent);assert(basename(actual).startsWith('secretary-video-reference-'));await rm(actual,{recursive:true,force:true});}
}
const probes=[];
const base={format:{duration:2},streams:[{index:0,codec_type:'video',width:320,height:240}]};
for(const duration of [0,-1,91,'NaN','Infinity',null,true,'0x2',[2],{},0.000000000000799999999999998,8/100000000000000.5,8/10000000000000.25,8/1000000000000.125,90,3])probes.push(['duration '+JSON.stringify(duration),{...clone(base),format:{duration}}]);
for(const streams of [null,{},[],Array(9).fill(base.streams[0]),[{...base.streams[0],index:'0'}],[{...base.streams[0],index:8}],[{...base.streams[0],width:'0x140',height:'240'}],[{...base.streams[0],width:1}],[{...base.streams[0],height:4097}],[base.streams[0],{index:1,codec_type:'video',width:2,height:2}],[base.streams[0],{index:1,codec_type:'audio'},{index:2,codec_type:'audio'}]])probes.push(['streams '+JSON.stringify(streams),{...clone(base),streams}]);
for(const audio of [{index:1,codec_type:'audio',start_time:-1},{index:1,codec_type:'audio',start_time:3},{index:1,codec_type:'audio',duration:1},{index:1,codec_type:'audio',duration:null},{index:1,codec_type:'audio',duration:0.02,start_time:'0.125'}])probes.push(['audio '+JSON.stringify(audio),{...clone(base),streams:[base.streams[0],audio]}]);
await add('ordered complete JPEG and raw PCM projection');
await add('MOV preserves original seven-bit ASCII tag admission',{highAscii:true});
await add('AVI preserves original seven-bit ASCII tag admission',{highAscii:true,type:'video/x-msvideo'});
for(const type of ['video/quicktime','video/webm','video/x-matroska','video/x-msvideo',' VIDEO/MP4 ;ignored','application/octet-stream'])await add('format '+type,{type,silent:true});
for(const [label,options]of [['signature',{badSignature:true}],['length',{wrongLength:true}],['hash',{wrongHash:true}],['oversized',{length:32*1024*1024+1}],['decoder identity',{changedDecoder:true}],['malformed probe',{invalidJson:true}],['BOM probe',{bomJson:true}],['no frames',{frames:0}],['frame gap',{frameGap:true}],['bad frame signature',{badFrame:true}],['oversized frame',{frames:1,frameSize:512*1024+1}],['odd audio',{pcmSize:3}],['empty audio',{pcmSize:0}]])await add(label,options);
for(const budget of [0,-1,1.5,6*1024*1024+1,1])await add('native budget '+budget,{actions:[{id:0,budget}]});
for(const [label,probe]of probes)await add(label,{probe});
for(const failAt of [1,2,3])for(const failure of ['media_video_decoder_failed','media_video_decode_failed','media_video_timeout','media_native_input_too_large'])await add('command failure '+failAt+' '+failure,{failAt,failure});
for(const scratch of [{entries:8},{badName:true},{entryFile:true},{files:10},{files:1,large:230*1024*1024}])await add('retained scratch '+JSON.stringify(scratch),{scratch});
await add('cache reuses immutable inputs and retains an over-budget successful projection',{actions:[{id:0,budget:1},{id:0,budget:5*1024*1024,mutate:true},{id:0,budget:5*1024*1024,name:'renamed.mp4'}]});
await add('cache evicts by insertion order without promoting hits',{silent:true,actions:[...Array.from({length:9},(_,id)=>({id,budget:5*1024*1024})),{id:1,budget:5*1024*1024},{id:9,budget:5*1024*1024},{id:1,budget:5*1024*1024}]});
await add('cache byte capacity independently evicts before entry capacity',{silent:true,frameSize:512*1024,compact:true,actions:[0,1,2,0].map(id=>({id,budget:6*1024*1024}))});
const schema=[];const sample=JSON.parse(rows[0].expected.results[0].value.inputs[0].text);
for(const change of [{},{durationSeconds:91},{width:1},{frames:[]},{limitations:['source_has_no_audio']},{frames:[{...sample.frames[0],inputOffset:2}]},{frames:[{...sample.frames[0],sampleTimeSeconds:1}]},{audio:null},{limitations:[...sample.limitations,'source_has_no_audio']}]){
 const value={...sample,...change};try{validateVideo('VideoProjection',value);schema.push({value,result:null});}catch(cause){schema.push({value,result:error(cause)});}
}
assert(rows[0].expected.results[0].value.kind==='video');assert.equal(rows.at(-3).expected.trace.length,3);assert.equal(rows.at(-2).expected.trace.length,22);assert.equal(rows.at(-1).expected.trace.length,8);
console.log(JSON.stringify({rows,schema}));
