import assert from 'node:assert/strict';
import {copyFileSync,cpSync,mkdirSync,readFileSync,readdirSync,realpathSync,writeFileSync} from 'node:fs';
import {dirname,join,relative,resolve,sep} from 'node:path';
import {createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {spawnSync} from 'node:child_process';
import {pdfBytes} from '../secretary-pdf-fixture.mjs';

const sha256=bytes=>createHash('sha256').update(bytes).digest('hex');

// Consume a verified asset-only archive in the real current-target launch fixture.
// Its manifest, package/lock subset and build stamps are intentionally excluded.
export function installRuntimeAssets(root,source){
  source=realpathSync(source);
  const dist=join(root,'dist'),before=readFileSync(join(dist,'build-info.json'));
  const pins=[];
  function catalogs(directory){
    for(const entry of readdirSync(directory,{withFileTypes:true})){
      const path=join(directory,entry.name);
      assert(!entry.isSymbolicLink());
      if(entry.isDirectory())catalogs(path);
      else if(entry.name.endsWith('.json')||entry.name==='LICENSE.codex'){
        const name=relative(join(source,'dist'),path),bytes=readFileSync(path);
        assert.deepEqual(bytes,readFileSync(resolve(name)),name);
        const destination=join(dist,name);mkdirSync(dirname(destination),{recursive:true});
        copyFileSync(path,destination);pins.push({path:'dist/'+name.replaceAll('\\','/'),bytes:bytes.length,sha256:sha256(bytes)});
      }
    }
  }
  catalogs(join(source,'dist/specs/native'));
  assert(pins.some(pin=>pin.path.endsWith('/catalog.json')));
  const adapter=readFileSync(join(source,'dist/services/secretary/pdf-worker.mjs'));
  assert.deepEqual(adapter,readFileSync('services/secretary/rust/pdf-worker.mjs'));
  const worker=join(dist,'services/secretary/pdf-worker.mjs');
  copyFileSync(join(source,'dist/services/secretary/pdf-worker.mjs'),worker);
  cpSync(join(source,'node_modules'),join(root,'node_modules'),{recursive:true,errorOnExist:true,force:false});

  const requireAsset=createRequire(join(root,'retained-asset-probe.cjs'));
  const lock=JSON.parse(readFileSync('package-lock.json','utf8'));
  const dependencyPins=['pdfjs-dist','@napi-rs/canvas'].map(name=>{
    const file=requireAsset.resolve(name+'/package.json');
    assert(file.startsWith(join(root,'node_modules')+sep));
    const value=JSON.parse(readFileSync(file,'utf8'));
    assert.equal(value.version,lock.packages['node_modules/'+name].version);
    return {path:relative(root,file).replaceAll('\\','/'),version:value.version,sha256:sha256(readFileSync(file))};
  });
  const canvas=requireAsset('@napi-rs/canvas'),image=canvas.createCanvas(2,2),context=image.getContext('2d');
  const addons=Object.keys(requireAsset.cache).filter(file=>file.endsWith('.node'));
  assert(addons.length>0);assert(addons.every(file=>file.startsWith(join(root,'node_modules')+sep)));
  const nativePins=addons.map(file=>({path:relative(root,file).replaceAll('\\','/'),bytes:readFileSync(file).length,sha256:sha256(readFileSync(file))}));
  context.fillStyle='#ff0000';context.fillRect(0,0,2,2);
  const png=image.toBuffer('image/png');assert.equal(png.subarray(0,8).toString('hex'),'89504e470d0a1a0a');
  const env={...process.env};delete env.NODE_PATH;delete env.NODE_OPTIONS;
  const pages=['Retained package first page.','Retained package second page.'];
  const pdf=spawnSync(process.execPath,[worker],{cwd:root,env,input:pdfBytes(pages),encoding:'utf8',timeout:35000,windowsHide:true});
  assert.ifError(pdf.error);assert.equal(pdf.signal,null);assert.equal(pdf.status,0,pdf.stderr);
  const envelope=JSON.parse(pdf.stdout);
  assert.equal(envelope.value?.format,'pdf');assert.equal(envelope.value.representation,'document_text');
  assert.deepEqual(envelope.value.sections,pages.map((text,index)=>({label:'Page '+(index+1),text})));
  assert.deepEqual(readFileSync(join(dist,'build-info.json')),before);
  const result={catalogs:pins,adapter:{sha256:sha256(adapter),bytes:adapter.length},dependencies:dependencyPins,
    canvas:{bytes:png.length,sha256:sha256(png),addons:nativePins},pdf:envelope,identityPreserved:true,
    qualification:'Actual current-target asset composition and private PDF/canvas execution; not a universal release, full installed acceptance or document OS-memory/retirement qualification.'};
  if(process.env.IVY_SECRETARY_RUNTIME_ASSET_RECEIPT)writeFileSync(process.env.IVY_SECRETARY_RUNTIME_ASSET_RECEIPT,JSON.stringify(result,null,2)+'\n',{flag:'wx'});
  return result;
}
