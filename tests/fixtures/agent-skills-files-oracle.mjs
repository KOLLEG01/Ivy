import {registerHooks} from 'node:module';
import {readFileSync} from 'node:fs';
import {mkdir,writeFile,readFile,rm,readdir,symlink,lstat} from 'node:fs/promises';
import {join,dirname,relative} from 'node:path';
import assert from 'node:assert/strict';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest,hashJson} from '../../dist/packages/contracts/src/canonical.js';
const hook=registerHooks({load(url,context,next){
  if(url.endsWith('/dist/services/agent-manager/src/environment.js'))return {format:'module',source:readFileSync(new URL(url),'utf8')+'\nexport {treeHash};\n',shortCircuit:true};
  return next(url,context);
}});
const {SkillsManager,treeHash}=await import('../../dist/services/agent-manager/src/environment.js');hook.deregister();
let bytes='';for await(const chunk of process.stdin)bytes+=chunk;const input=JSON.parse(bytes);
const hive='https://fixture.example',hostId='fixture',authority=hashJson({hive,hostId});
const names=['SKILL.md','nested','notes.txt','é.txt','e\u0301.txt','Å.txt','A\u030A.txt','empty','a.txt','z.txt','link','bad\ufffd\ufffd\ufffdname'];
const pairs=names.flatMap(left=>names.map(right=>({left,right,result:Math.sign(left.localeCompare(right))})));
const defaults=(enabled=true)=>({schemaVersion:1,hostId:null,enabled,files:[{path:'alpha/SKILL.md',content:'default\né'},{path:'alpha/nested/notes.txt',content:'contents'},{path:'beta/SKILL.md',content:'second'}]});
const applyModes=['fresh','disabled','empty','owned-same','owned-change','owned-edit','owned-missing','unowned','second-collision','removed-edit','invalid-state','state-directory','root-link','state-root-link','owned-link','oversized-owned','relative','missing-home','read-failure','bad-document'];
const bootstrapModes=['fresh','disabled','invalid-state','empty-state','null-state','state-directory','oversized-state','missing-home'];
const treeModes=['ordered','link','oversized','file','missing',...(process.platform==='win32'?['lone-surrogate']:[])];
const missing={async request(method){assert.equal(method,'objects.stat');throw new IvyError('not_found','fixture missing');}};
const normalize=(value,root)=>typeof value==='string'?value.replaceAll(root,'<root>').replaceAll('\\','/'):Array.isArray(value)?value.map(v=>normalize(v,root)):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v,root)])):value;
function status(manager){assert(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(manager.status.observedAt));const{observedAt,...value}=manager.status;return value;}
async function writeTree(path,label='old') {await mkdir(join(path,'nested'),{recursive:true});await writeFile(join(path,'SKILL.md'),label);await writeFile(join(path,'nested/notes.txt'),'contents');}
async function prepare(root,mode,bootstrap=false){
  const home=join(root,'home'),target=join(root,'skills'),stateRoot=join(root,'.ivy-agent-skills-'+digest(target).slice(7,23)),statePath=join(stateRoot,'state.json'),doc=defaults(mode!=='disabled');
  await mkdir(root,{recursive:true});if(mode!=='missing-home')await mkdir(home);
  if(mode==='empty')doc.files=[];
  if(mode==='relative')return {home,target:'relative',statePath,defaults:doc};
  if(mode==='bad-document')doc.files[0].path='../SKILL.md';
  if(mode==='root-link'||mode==='state-root-link'){const real=join(root,'real');await mkdir(real);await symlink(real,mode==='root-link'?target:stateRoot,process.platform==='win32'?'junction':'dir');}
  if(mode.startsWith('owned-')||mode==='oversized-owned'){
    await writeTree(join(target,'alpha'),mode==='owned-same'?'default\né':'old');
    await mkdir(stateRoot);const state={schemaVersion:2,authority,applied:{alpha:await treeHash(join(target,'alpha'))},appliedVersion:'prior'};await writeFile(statePath,JSON.stringify(state));
    if(mode==='owned-edit')await writeFile(join(target,'alpha/SKILL.md'),'local edit');
    if(mode==='owned-missing')await rm(join(target,'alpha'),{recursive:true});
    if(mode==='owned-link'){await mkdir(join(root,'outside'));await symlink(join(root,'outside'),join(target,'alpha/link'),process.platform==='win32'?'junction':'dir');}
    if(mode==='oversized-owned')await writeFile(join(target,'alpha/SKILL.md'),Buffer.alloc(256*1024+1));
  }
  if(mode==='unowned'||mode==='second-collision')await writeTree(join(target,mode==='unowned'?'alpha':'beta'),'outside management');
  if(mode==='removed-edit'){await writeTree(join(target,'removed'),'keep local edit');await mkdir(stateRoot);await writeFile(statePath,JSON.stringify({schemaVersion:2,authority,applied:{removed:'old hash'},appliedVersion:'prior'}));}
  if(['invalid-state','empty-state','null-state','state-directory','oversized-state'].includes(mode)){
    await mkdir(stateRoot);if(mode==='state-directory')await mkdir(statePath);else await writeFile(statePath,mode==='invalid-state'?'{':mode==='empty-state'?'':mode==='null-state'?'null':Buffer.alloc(4*1024*1024+1,32));
  }
  return {home,target,statePath,defaults:doc};
}
async function snapshot(root,provided){
  const target=join(root,'skills'),statePath=join(root,'.ivy-agent-skills-'+digest(target).slice(7,23),'state.json');
  const state=await readFile(statePath,'utf8').then(text=>{try{return JSON.parse(text);}catch{return {rawDigest:digest(text),bytes:Buffer.byteLength(text)};}},error=>({error:IvyError.from(error).code}));
  const trees={};for(const name of ['alpha','beta','removed','ivy-managed-alpha','ivy-managed-beta'])trees[name]=await treeHash(join(target,name)).catch(error=>({error:IvyError.from(error).code}));
  const leftovers=[];for(const name of await readdir(dirname(statePath)).catch(()=>[]))if(name.startsWith('.stage-')||name.startsWith('.retired-'))leftovers.push(name);
  return normalize({state,trees,leftovers,...provided},root);
}
async function treeFixture(root,mode){
  await mkdir(root,{recursive:true});const target=join(root,'tree');
  if(mode==='missing')return target;
  if(mode==='file'){await writeFile(target,'file');return target;}
  await writeTree(target,'tree');
  if(mode==='ordered')for(const name of ['é.txt','e\u0301.txt','Å.txt','A\u030A.txt','a.txt','z.txt'])await writeFile(join(target,name),name);
  if(mode==='link'){await mkdir(join(root,'outside'));await symlink(join(root,'outside'),join(target,'link'),process.platform==='win32'?'junction':'dir');}
  if(mode==='oversized')await writeFile(join(target,'SKILL.md'),Buffer.alloc(256*1024+1));
  if(mode==='lone-surrogate')await writeFile(join(target,'bad\ud800name'),'unpaired native name');
  return target;
}
if(input.action==='catalog')console.log(JSON.stringify({applyModes,bootstrapModes,treeModes,pairs,defaults:defaults()}));
else if(input.action==='prepare')console.log(JSON.stringify(await prepare(input.root,input.mode,input.bootstrap)));
else if(input.action==='snapshot')console.log(JSON.stringify(await snapshot(input.root,input.provided)));
else if(input.action==='treePrepare')console.log(JSON.stringify(await treeFixture(input.root,input.mode)));
else if(input.action==='treeRun'){const target=await treeFixture(input.root,input.mode);try{console.log(JSON.stringify({result:await treeHash(target)}));}catch(error){console.log(JSON.stringify({error:IvyError.from(error).toWire()}));}}
else if(input.action==='lifecycle'){
  const prepared=await prepare(input.root,'fresh'),manager=new SkillsManager(prepared.home,prepared.target,hostId,hive,prepared.defaults),states=[status(manager)];
  await manager.synchronize(missing);states.push(status(manager));
  await manager.synchronize({async request(){throw new IvyError('service_unavailable','fixture failure','unknown');}});states.push(status(manager));
  const good=await readFile(join(prepared.target,'alpha/SKILL.md'));await writeFile(join(prepared.target,'alpha/SKILL.md'),'local edit');await manager.synchronize(missing);states.push(status(manager));
  await writeFile(join(prepared.target,'alpha/SKILL.md'),good);await manager.synchronize(missing);await manager.close();states.push(status(manager));
  let release,entered;const gate=new Promise(r=>release=r),started=new Promise(r=>entered=r),calls=[];
  const held={async request(method,params){calls.push(method);if(calls.length===1){entered();await gate;}return missing.request(method,params);}};
  const first=manager.synchronize(held);await started;let secondCalls=0,closed=false;
  const second=manager.synchronize({async request(){secondCalls++;throw Error('unused');}}),close=manager.close().then(()=>closed=true);assert.equal(first,second);await Promise.resolve();assert.equal(closed,false);
  const during=status(manager);release();await Promise.all([first,second,close]);assert.equal(secondCalls,0);
  manager.synchronize(missing);await manager.close();
  console.log(JSON.stringify(normalize({states,during,after:status(manager),calls,secondCalls,closed},input.root)));
}else if(input.action==='migrationPrepare'){
  const{home,target,statePath}=await prepare(input.root,'fresh'),version=input.version??1,inPlace=input.inPlace??false,source=inPlace?statePath:join(home,'.ivy-agent-skills/state.json');
  await mkdir(dirname(source),{recursive:true});const applied={};
  for(const name of ['alpha','beta']){const old=version===1?'ivy-managed-'+name:name;await writeTree(join(target,old),'retained '+name);applied[old]=await treeHash(join(target,old));}
  const state={schemaVersion:version,authority,applied,appliedVersion:'retained version',extra:{text:'retained 😀',count:7}};
  const sourceBytes=Buffer.from(JSON.stringify(state,null,2)+'\n');await writeFile(source,sourceBytes);
  console.log(JSON.stringify({home,target,statePath,source,sourceDigest:digest(sourceBytes),state,pairs,defaults:defaults()}));
}else{
  const prepared=await prepare(input.root,input.mode,input.bootstrap),manager=new SkillsManager(prepared.home,prepared.target,hostId,hive,prepared.defaults);let error=null;
  try{if(input.bootstrap)await manager.bootstrap();else await manager.synchronize(input.mode==='read-failure'?{async request(){throw new IvyError('service_unavailable','fixture failure','unknown');}}:missing);}catch(cause){error=IvyError.from(cause).toWire();}
  await manager.close();console.log(JSON.stringify(await snapshot(input.root,{status:status(manager),error})));
}
