import {existsSync,readFileSync,readdirSync,lstatSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import assert from 'node:assert/strict';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
function snapshotTree(snapshot){
  const files=[];
  function walk(directory){
    for(const name of readdirSync(directory).sort()){
      const file=path.join(directory,name),s=lstatSync(file);
      if(s.isDirectory())walk(file);
      else files.push({path:path.relative(snapshot.sourceRoot,file).split(path.sep).join('/'),hex:readFileSync(file).toString('hex'),...(process.platform==='win32'?{}:{mode:s.mode&0o777})});
    }
  }
  walk(snapshot.sourceRoot);
  return files;
}
try{
  let value;
  if(input.kind==='capture'){
    const {captureSource}=await import('../../packages/host-runtime/src/source.ts');
    const snapshot=await captureSource(input.source,input.config);
    const manifest=JSON.parse(readFileSync(snapshot.manifestPath,'utf8'));
    assert.deepEqual(manifest,{schemaVersion:1,snapshot});
    value={files:snapshotTree(snapshot),originalRoot:snapshot.originalRoot};
  }else if(input.kind==='cli'){
    const {cli}=await import('../../packages/cli/src/main.ts');
    value=await cli(input.args);
  }else throw Error('Unknown action oracle mode');
  process.stdout.write(JSON.stringify({ok:true,value}));
}catch(error){process.stdout.write(JSON.stringify({ok:false,error:IvyError.from(error).toWire()}));}
