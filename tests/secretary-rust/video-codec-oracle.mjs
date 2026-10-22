import assert from 'node:assert/strict';
import {readFileSync,writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {after} from 'node:test';
import {createHash} from 'node:crypto';
import {instances} from './video-codec-reference.mjs';
const original=readFileSync(new URL('../secretary-video.test.mjs',import.meta.url),'utf8');
const originalImport="from '../dist/services/secretary/src/media-video.js'";
assert.equal(original.split(originalImport).length,2);
const shim=new URL('./video-codec-reference.mjs',import.meta.url).href;
let selected=original.replace(originalImport,'from '+JSON.stringify(shim));
selected=selected.replace(/from '\.\.\/dist\/([^']+)'/g,(_,path)=>'from '+JSON.stringify(pathToFileURL(resolve(process.env.IVY_SECRETARY_ORACLE,path)).href));
// Reuse the original clip generator and gates for two distinct real-container risks.
const extra=`
test('actual excessive dimensions and truncated container retain original codec errors and clean scratch',real,async t=>{
  const f=await configured(t),wide=await f.clip('too-wide.mp4',{size:'4098x2',duration:1,rate:1,audio:false});
  assert.equal((await f.decoder.prepare(source(wide),wide,5*1024*1024)).code,'media_video_dimensions_unsupported');
  const good=await f.clip('corrupt-source.mp4',{audio:false}),broken=good.subarray(0,32);
  assert.equal((await f.decoder.prepare(source(broken),broken,5*1024*1024)).code,'media_video_decode_failed');
  assert.deepEqual(await readdir(join(f.directory,'work')),[]);
});
`;
after(()=>{
  assert(instances.length>0);
  const packet={schemaVersion:1,originalTestSourceHash:createHash('sha256').update(original).digest('hex'),extraCaseSource:extra,configuration:JSON.parse(readFileSync(process.env.IVY_TEST_VIDEO_CONFIG,'utf8')),instances};
  writeFileSync(process.env.IVY_SECRETARY_CODEC_CORPUS,JSON.stringify(packet)+'\n',{flag:'wx'});
});
await import('data:text/javascript;base64,'+Buffer.from(selected+extra).toString('base64'));
