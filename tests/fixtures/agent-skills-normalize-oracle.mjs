// Export the original private preflight without changing its implementation.
import {registerHooks} from 'node:module';
import {readFileSync} from 'node:fs';
import {digest} from '../../dist/packages/contracts/src/canonical.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
const hook=registerHooks({load(url,context,next){
  if(url.endsWith('/dist/services/agent-manager/src/environment.js'))return {format:'module',source:readFileSync(new URL(url),'utf8')+'\nexport { normalizeSkills };\n',shortCircuit:true};
  return next(url,context);
}});
const {normalizeSkills}=await import('../../dist/services/agent-manager/src/environment.js');
hook.deregister();
const cases=[],file=(path,text='',repeat=1)=>({path,text,repeat});
const add=(name,files,enabled=true)=>cases.push({name,files,enabled});
add('empty',[]);
add('disabled-invalid',[file('../escape','x')],false);
add('inherit-invalid',[file('../escape','x')],null);
add('ordered-groups',[file('z/SKILL.md','z'),file('a/SKILL.md','a'),file('z/nested/run.ps1','line\r\n'),file('a/2.txt','\ufeff😀\ud800\udc00\ud800')]);
for(const path of ['', '/s/SKILL.md','s\\SKILL.md','s/','s//SKILL.md','s/./SKILL.md','s/../SKILL.md','../s/SKILL.md','C:/s/SKILL.md','C:s/SKILL.md','s','S/SKILL.md','s_/SKILL.md','0s/SKILL.md','ä/SKILL.md','a'.repeat(65)+'/SKILL.md','a'.repeat(64)+'/SKILL.md','s/skill.md','s/nested/SKILL.md','s/😀.txt','s/a\0b','s/\ud800'])add('path:'+path,[file('s/SKILL.md'),file(path,'content')]);
add('duplicate',[file('s/SKILL.md'),file('s/SKILL.md','new')]);
add('case-distinct',[file('s/SKILL.md'),file('s/FILE'),file('s/file')]);
add('unicode-distinct',[file('s/SKILL.md'),file('s/\ud800','\ud800'),file('s/\ufffd','\ufffd')]);
add('file-exact',[file('s/SKILL.md','a',256*1024)]);
add('file-over',[file('s/SKILL.md','a',256*1024+1)]);
add('utf8-exact',[file('s/SKILL.md','😀',64*1024)]);
add('utf8-over',[file('s/SKILL.md','😀',64*1024+1)]);
add('replacement-over',[file('s/SKILL.md','\ud800',87382)]);
add('total-exact',Array.from({length:16},(_,i)=>file('s/'+(i===0?'SKILL.md':i+'.txt'),'a',256*1024)));
add('total-over',[...cases.at(-1).files,file('s/extra','x')]);
add('missing-skill-order',[file('z/file'),file('a/file')]);
add('missing-skill-case',[file('s/skill.md')]);
add('nested-skill-only',[file('s/nested/SKILL.md')]);
add('duplicate-before-size',[file('s/SKILL.md'),file('s/SKILL.md','a',256*1024+1)]);
add('missing-after-size',[file('s/file','a',256*1024+1)]);
const rows=cases.map(input=>{
  const document={schemaVersion:1,hostId:null,enabled:input.enabled,files:input.files.map(f=>({path:f.path,content:f.text.repeat(f.repeat)}))};
  let expected;
  try{expected={result:[...normalizeSkills(document)].map(([name,files])=>({name,files:[...files].map(([path,bytes])=>({path,bytes:bytes.length,hash:digest(bytes)}))}))};}
  catch(error){expected={error:IvyError.from(error).toWire()};}
  return {...input,expected};
});
process.stdout.write(JSON.stringify({platform:process.platform,rows}));
