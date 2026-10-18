import {readFileSync,readdirSync,existsSync} from 'node:fs';
import {createRequire,registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import {schemaSamples} from './schema-samples.mjs';
registerHooks({resolve(specifier,context,next){
  if(specifier.endsWith('.js')&&specifier.startsWith('.')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {validateAutomation}=await import('../../packages/contracts/src/automation-validation.ts');
const {validateChat}=await import('../../packages/contracts/src/chat-validation.ts');
const {validateSecretary}=await import('../../packages/contracts/src/secretary-validation.ts');
const {validateAgent}=await import('../../packages/contracts/src/agent-validation.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {validateShared,validateTransport}=await import('../../packages/contracts/src/core-validation.ts');
const {validateComponent}=await import('../../packages/contracts/src/component-validation.ts');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const ajv=new Ajv2020({strict:false,allErrors:false,validateFormats:false,ownProperties:true});
const names=['hive-wire','hive-operations','host','agent','automation','chat','secretary','component','hive-transport'];
const documents=names.map(name=>JSON.parse(readFileSync(new URL('../../specs/schemas/'+name+'.schema.json',import.meta.url),'utf8')));
for(const document of documents)ajv.addSchema(document);
const domains={agent:validateAgent,host:validateHost,shared:validateShared,transport:validateTransport,automation:validateAutomation,chat:validateChat,secretary:validateSecretary,component:(_,v)=>validateComponent(v)};
const rows=[];
function add(domain,name,input){
  let error=null;try{domains[domain](name,JSON.parse(input));}catch(cause){const e=IvyError.from(cause);error={code:e.code,message:e.message,outcome:e.outcome};}
  rows.push({domain,name,input,error});
}
for(const row of schemaSamples(ajv,documents,documents.filter((_,i)=>![1,7].includes(i)))){
  const [resource,fragment]=row.reference.split('#/$defs/'),source=resource.split('/').at(-1).split('.')[0],domain=({'hive-wire':'shared','hive-transport':'transport'})[source]??source;
  add(domain,fragment,row.input);
  if(row.valid){const value=JSON.parse(row.input);
    const paths=[];function walk(v,path=[],depth=0){if(!v||typeof v!=='object'||depth>6)return;for(const key of Object.keys(v).slice(0,12)){paths.push([...path,key]);walk(v[key],[...path,key],depth+1);}}walk(value);
    for(const path of paths.slice(0,64))for(const replacement of [null,{},[],false,0,'','\ud800','😀']){const changed=structuredClone(value);let target=changed;for(const key of path.slice(0,-1))target=target[key];target[path.at(-1)]=replacement;add(domain,fragment,JSON.stringify(changed));}
  }
}
for(const domain of ['automation','chat','secretary','host','agent','shared','transport']){
  const doc=documents[names.indexOf(({shared:'hive-wire',transport:'hive-transport'})[domain]??domain)],known=Object.keys(doc.$defs)[0];
  for(const name of ['Absent','constructor','toString','__proto__','constructor/prototype','constructor/name','constructor/length','constructor/foo','constructor/constructor','constructor/caller','constructor/arguments','constructor/name/0','constructor/name/length','constructor/name/__proto__/constructor','constructor/prototype/__proto__','constructor/prototype/__proto__/x','__proto__/foo','%63onstructor','constructor%2Fprototype','constructor~1prototype','constructor/#','constructor/',known,known.replace(/^./,c=>'%'+c.charCodeAt(0).toString(16)),known+'/type',known+'/properties',known+'%2Ftype','%','%ff','..',''])for(const input of ['{}','null','"x"'])add(domain,name,input);
  for(const input of ['9007199254740992','1e400','-1e400'])add(domain,'Absent',input);
  if(!['automation','chat','secretary','host'].includes(domain))continue;
  for(const [shape,input] of [['deep','['.repeat(129)+'0'+']'.repeat(129)],['wide','[0'+',0'.repeat(400000)+']'],['large','"'+'x'.repeat(32*1024*1024)+'"']]){add(domain,'Absent',input);rows.at(-1).input=null;rows.at(-1).shape=shape;}
}
// Real checked-in component plans cover optional launch, health, native and UI fields.
for(const parent of ['services','ui'])for(const entry of readdirSync(new URL('../../'+parent+'/',import.meta.url),{withFileTypes:true})){
  if(!entry.isDirectory())continue;const file=new URL('../../'+parent+'/'+entry.name+'/deploy.json',import.meta.url);if(!existsSync(file))continue;
  const value=JSON.parse(readFileSync(file,'utf8'));add('component','',JSON.stringify(value));
  for(const key of Object.keys(value)){const missing={...value};delete missing[key];add('component','',JSON.stringify(missing));for(const replacement of [null,{},[],false,0,'','\ud800','😀'])add('component','',JSON.stringify({...value,[key]:replacement}));}
  if(value.requirements)for(const node of [null,'>=24.18.0 <25.0.0','>=24.18.0 <25',0])add('component','',JSON.stringify({...value,requirements:{...value.requirements,node}}));
}
for(const input of ['null','false','0','""','[]','{}','1e400','9007199254740992'])add('component','',input);
process.stdout.write(JSON.stringify(rows));
