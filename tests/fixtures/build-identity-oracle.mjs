import {mkdtempSync,writeFileSync,mkdirSync,rmSync,existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import {buildIdentity} from '../../packages/host-runtime/src/build-identity.mjs';
registerHooks({resolve(s,c,next){if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return next(u.href,c);}return next(s,c);}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const root=mkdtempSync(join(tmpdir(),'ivy-build-identity-')),rows=[];
const valid='sha256:'+'a'.repeat(64);
function add(label,text,supplied=valid,shape=null){
  const bytes=Buffer.isBuffer(text)?text:Buffer.from(text??'');
  const directory=join(root,String(rows.length));mkdirSync(directory);
  if(shape==='directory')mkdirSync(join(directory,'package.json'));
  else if(shape!=='missing')writeFileSync(join(directory,'package.json'),bytes);
  if(supplied===null)delete process.env.IVY_BUILD_ID;else process.env.IVY_BUILD_ID=supplied;
  let result;
  try{const value=buildIdentity(directory);if(supplied!==valid){if(!/^sha256:[0-9a-f]{64}$/.test(value.buildId))throw Error('Invalid generated identity');value.buildId='<opaque>';}
    result={ok:true,value};
  }catch(error){result={ok:false,error:IvyError.from(error).toWire()};}
  rows.push({label,shape,bytes:shape?null:[...bytes],supplied,opaque:supplied!==valid,result:JSON.stringify(result)});
}
try{
  for(const [label,text] of [
    ['ordinary','{"version":"0.2.40"}'],['missing-version','{}'],['root-array','[]'],['root-string','"version"'],['root-number','23'],['root-boolean','true'],['root-null','null'],
    ['null-version','{"version":null}'],['boolean-version','{"version":false}'],['number-version','{"version":1.25}'],['unsafe-number','{"version":9007199254740993}'],['overflow','{"version":1e400}'],['negative-zero','{"version":-0}'],
    ['surrogates','{"version":"a\\ud800b\\udfff😀"}'],['duplicate','{"version":"old","version":"new"}'],['object-version','{"version":{"valueOf":0,"x":2}}'],['array-version','{"version":[null,1,[2,{},null],"x"]}'],
    ['blocked-string-conversion','{"version":{"toString":null}}'],['nested-blocked-conversion','{"version":[{"toString":"x"}]}'],['irrelevant-nested-property','{"version":{"x":{"toString":null}}}'],
    ['prototype-key','{"__proto__":{"version":"inherited"}}'],['empty',''],['malformed','{"version":'],['bom','\ufeff{"version":"x"}']
  ])for(const id of [valid,null])add(label,text,id);
  for(const id of ['',valid.toUpperCase(),valid+'\n',valid+'\r',valid+'x',' '+valid,'sha256:'+'g'.repeat(64),'sha256:'+'a'.repeat(63)])add('invalid-supplied','{"version":"0.2.40"}',id);
  for(const bytes of [[0xff],[0xe2,0x82],[0xed,0xa0,0x80],[0xc0,0xaf],[0xf4,0x90,0x80,0x80]])add('replacement-utf8',Buffer.concat([Buffer.from('{"version":"'),Buffer.from(bytes),Buffer.from('"}')]),valid);
  for(const id of [valid,null]){
    add('large-package','{"version":"x","unused":"'+'x'.repeat(1048577)+'"}',id,'large');
    add('deep-package','{"version":"x","unused":'+'['.repeat(256)+'0'+']'.repeat(256)+'}',id,'deep');
    add('wide-package','{"version":"x","unused":[0'+',0'.repeat(400000)+']}',id,'wide');
    add('missing-package',null,id,'missing');add('directory-package',null,id,'directory');
  }
  // Fresh opaque identities must not become a source-content cache key.
  add('fresh-1','{"version":"0.2.40"}',null);add('fresh-2','{"version":"0.2.40"}',null);
  process.stdout.write(JSON.stringify(rows));
}finally{rmSync(root,{recursive:true,force:true});}
