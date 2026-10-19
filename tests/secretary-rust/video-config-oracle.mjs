import assert from 'node:assert/strict';
import {mkdtemp,writeFile,mkdir,realpath,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';import {join,dirname,basename} from 'node:path';
import {load} from './inbox-fixtures.mjs';
const {loadVideoConfiguration}=await load('services/secretary/src/media-video.js');
const {digest,IvyError}=await load('packages/sdk/src/node.js');
const rows=[];
const normalize=(value,root)=>typeof value==='string'?value.replaceAll(root,'<root>'):Array.isArray(value)?value.map(v=>normalize(v,root)):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v,root)])):value;
for(const kind of ['missing','absent','null','valid','extra','version','relative','decoder-hash','probe-hash','empty','directory','missing-executable']){
 const root=await mkdtemp(join(tmpdir(),'secretary-video-config-reference-')),parent=await realpath(tmpdir());
 try{
  await writeFile(join(root,'decoder.exe'),kind==='empty'?'':'decoder');await writeFile(join(root,'probe.exe'),'probe');
  const video={schemaVersion:kind==='version'?2:1,ffmpeg:{path:kind==='relative'?'decoder.exe':join(root,'decoder.exe'),hash:digest(kind==='decoder-hash'?'wrong':'decoder')},ffprobe:{path:join(root,'probe.exe'),hash:digest(kind==='probe-hash'?'wrong':'probe')}};
  if(kind==='extra')video.unexpected=true;
  if(kind==='directory'){await mkdir(join(root,'directory'));video.ffmpeg.path=join(root,'directory');}
  if(kind==='missing-executable')video.ffmpeg.path=join(root,'missing.exe');
  const configuration={schemaVersion:1,...(kind==='absent'?{}:{video:kind==='null'?null:video})};
  if(kind!=='missing')await writeFile(join(root,'config.json'),JSON.stringify(configuration),{mode:0o600});
  let expected;try{expected={value:await loadVideoConfiguration(join(root,'config.json'))};}catch(cause){const error=IvyError.from(cause);expected={code:error.code,outcome:error.outcome};}
  rows.push(normalize({kind,configuration,expected},root));
 }finally{const actual=await realpath(root);assert.equal(dirname(actual),parent);assert(basename(actual).startsWith('secretary-video-config-reference-'));await rm(actual,{recursive:true,force:true});}
}
console.log(JSON.stringify(rows));
