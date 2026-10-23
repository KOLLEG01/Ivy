import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,readdirSync,lstatSync,mkdirSync,rmSync} from 'node:fs';
import {join,resolve,relative} from 'node:path';
import {gunzipSync,gzipSync} from 'node:zlib';
import {execFileSync} from 'node:child_process';
import {randomFillSync} from 'node:crypto';
import {openSync,writeSync,closeSync} from 'node:fs';
import {stageNativePayloads} from '../../tools/build/stage-native-payloads.mjs';
import {rustBackendTargets} from '../../tools/build/build-rust-backend.mjs';
const input=JSON.parse(readFileSync(0,'utf8'));
const text=bytes=>bytes.toString('utf8').replace(/\0.*$/s,'');
const octal=bytes=>parseInt(text(bytes).trim()||'0',8);
function headers(archive){
  const bytes=gunzipSync(readFileSync(archive)),entries=[];let longName,pax={};
  for(let p=0;p+512<=bytes.length;){
    const h=bytes.subarray(p,p+512);if(h.every(b=>b===0))break;
    const size=octal(h.subarray(124,136)),type=String.fromCharCode(h[156]);
    const prefix=h.subarray(257,263).equals(Buffer.from('ustar\0'))?text(h.subarray(345,500)):'',name=(prefix?prefix+'/':'')+text(h.subarray(0,100));
    const data=bytes.subarray(p+512,p+512+size),next=p+512+Math.ceil(size/512)*512;
    if(type==='L')longName=text(data);
    else if(type==='x'||type==='g'){
      for(let at=0;at<data.length;){const end=data.indexOf(32,at),length=Number(data.subarray(at,end).toString());assert.ok(length>0);const field=data.subarray(end+1,at+length-1).toString('utf8'),equal=field.indexOf('=');pax[field.slice(0,equal)]=field.slice(equal+1);at+=length;}
    }else{entries.push({name:(pax.path??longName??name).replace(/^\.\//,''),type,mode:octal(h.subarray(100,108)),size,data,raw:bytes.subarray(p,next)});longName=undefined;pax={};}
    p=next;
  }
  return{bytes,entries};
}
function tree(root){
  const files=[];
  function walk(directory){for(const name of readdirSync(directory).sort()){const file=join(directory,name),s=lstatSync(file);assert.equal(s.isSymbolicLink(),false);if(s.isDirectory())walk(file);else{assert.ok(s.isFile());files.push({path:relative(root,file).replaceAll('\\','/'),hex:readFileSync(file).toString('hex'),...(process.platform==='win32'?{}:{mode:s.mode&0o777})});}}}
  walk(root);return files.sort((a,b)=>a.path<b.path?-1:a.path>b.path?1:0);
}
function entry(name,data=Buffer.alloc(0),type='0',link=''){
  const h=Buffer.alloc(512);h.write(name);h.write('0000644\0',100);h.write('0000000\0',108);h.write('0000000\0',116);
  h.write(data.length.toString(8).padStart(11,'0')+'\0',124);h.write('00000000000\0',136);h.fill(32,148,156);h.write(type,156);h.write(link,157);h.write('ustar\0',257);h.write('00',263);
  h.write(h.reduce((sum,b)=>sum+b,0).toString(8).padStart(6,'0')+'\0 ',148);
  return Buffer.concat([h,data,Buffer.alloc((512-data.length%512)%512)]);
}
let result;
if(input.mode==='compare'){
  const source=tree(input.source),first=tree(input.first),second=tree(input.second);
  assert.deepEqual(first,source);assert.deepEqual(second,source);
  result={equal:true,files:source,headers:headers(input.archive).entries.map(({data,raw,...h})=>h)};
}else if(input.mode==='originalCreate'){
  result=JSON.parse(execFileSync(process.execPath,['--experimental-transform-types','--disable-warning=ExperimentalWarning','tests/fixtures/host-config-oracle.mjs'],{input:JSON.stringify({kind:'packageArchive',operation:'createPackageArchive',args:[input.candidate,{}]}),encoding:'utf8'}));
}else if(input.mode==='headers'){
  result={headers:headers(input.archive).entries.map(({data,raw,...h})=>h)};
}else if(input.mode==='mutate'){
  const archive=gunzipSync(readFileSync(input.archive)),blocks=[];
  for(let p=0;p+512<=archive.length;){
    const h=archive.subarray(p,p+512);if(h.every(b=>b===0))break;
    const size=octal(h.subarray(124,136)),end=p+512+Math.ceil(size/512)*512,name=text(h.subarray(0,100)).replace(/^\.\//,'');
    if(input.case==='component-link'&&name==='component.json')blocks.push(entry('component.json',Buffer.alloc(0),'2','artifact/manifest.json'));
    else if(input.case==='artifact-link'&&name==='artifact/')blocks.push(entry('artifact',Buffer.alloc(0),'2','../outside'));
    else if(input.case==='payload-link'&&name==='artifact/dist/main.js')blocks.push(entry('artifact/dist/main.js',Buffer.alloc(0),'2','health.js'));
    else blocks.push(archive.subarray(p,end));p=end;
  }
  if(input.case==='unexpected-root')blocks.push(entry('extra',Buffer.from('unexpected')));
  if(input.case==='parent-path')blocks.push(entry('../escape',Buffer.from('contained fixture only')));
  if(input.case==='backslash')blocks.push(entry('artifact\\escape',Buffer.from('unsupported path')));
  if(input.case==='duplicate')blocks.push(entry('artifact/dist/main.js',readFileSync(join(input.root,'artifact/dist/main.js'))));
  if(input.case==='entry-limit')for(let i=0;i<50001;i++)blocks.push(entry('artifact/file-'+i));
  if(input.case==='listing-limit')for(let i=0;i<6000;i++){const name='artifact/'+('a'.repeat(1790))+i;blocks.push(entry('././@LongLink',Buffer.from(name+'\0'),'L'));blocks.push(entry('artifact/placeholder'));}
  writeFileSync(input.archive,gzipSync(Buffer.concat([...blocks,Buffer.alloc(1024)])));result={changed:true};
}else if(input.mode==='large'){
  const file=openSync(input.path,'w'),bytes=Buffer.alloc(1024*1024);
  try{for(let i=0;i<257;i++){randomFillSync(bytes);writeSync(file,bytes);}}finally{closeSync(file);}
  result={bytes:257*1024*1024};
}else if(input.mode==='stage'){
  const root=resolve(input.root),component=input.plan.componentId,identity={schemaVersion:1,version:'9.9.9',buildId:input.plan.buildId};
  mkdirSync(join(root,'services',component),{recursive:true});writeFileSync(join(root,'services',component,'deploy.json'),JSON.stringify(input.plan));
  const payloads=[];
  for(const[target,platform]of Object.entries(rustBackendTargets)){
    const outputDirectory=join(root,'payloads',platform);mkdirSync(outputDirectory,{recursive:true});writeFileSync(join(outputDirectory,'build-info.json'),JSON.stringify(identity));
    const files=['ivy-'+component,'ivy-health','ivy-extra'].map(name=>{const path=join(outputDirectory,name+(platform.startsWith('win32-')?'.exe':''));writeFileSync(path,Buffer.from('nonexecutable fixture bytes:'+platform+'/'+name));return path;});
    payloads.push({target,platform,component,identity,outputDirectory,files,instructionFiles:[]});
  }
  rmSync(input.outputRoot,{recursive:true,force:true});
  result=stageNativePayloads({component,root,outputRoot:input.outputRoot,identity,payloads,requiredBinaries:{win32:['ivy-extra'],linux:['ivy-extra']}});
}else throw Error('Unknown native package fixture mode');
process.stdout.write(JSON.stringify(result));
