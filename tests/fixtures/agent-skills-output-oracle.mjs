import {registerHooks} from 'node:module';
import {readFileSync} from 'node:fs';
import {mkdtemp,mkdir,writeFile,readFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
import assert from 'node:assert/strict';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest,hashJson} from '../../dist/packages/contracts/src/canonical.js';
const hook=registerHooks({load(url,context,next){
  if(url.endsWith('/dist/services/agent-manager/src/environment.js'))return {format:'module',source:readFileSync(new URL(url),'utf8')+'\nexport {normalizeSkills,treeHash};\n',shortCircuit:true};
  return next(url,context);
}});
const original=await import('../../dist/services/agent-manager/src/environment.js');hook.deregister();
const hive='https://fixture.example',hostId='fixture',authority=hashJson({hive,hostId});
const file=(path,content)=>({path,content});
const document=(scope,enabled,label='default')=>({schemaVersion:1,hostId:scope,enabled,files:[file('alpha/SKILL.md',label+'\né'),file('alpha/nested/readme.txt','contents')]});
const missing={async request(){throw new IvyError('not_found','fixture missing');}};
const root=await mkdtemp(join(tmpdir(),'ivy-skills-output-')),preparation=[],ownership=[];
async function writeTree(path,files){for(const [local,bytes] of files){const target=join(path,...local.split('/'));await mkdir(dirname(target),{recursive:true});await writeFile(target,bytes);}}
async function snapshot(root,names){const result={};for(const name of names)result[name]=await original.treeHash(join(root,name)).then(hash=>hash,e=>({error:IvyError.from(e).code}));return result;}
try {
  const cases=[];
  for(const enabled of [true,false])for(const global of ['missing',null,false,true])for(const host of ['missing',null,false,true])cases.push({enabled,global,host});
  for(const mode of ['global-stat-fail','global-read-fail','host-stat-fail','host-read-fail','global-scope','host-scope','global-contract','host-version','host-schema','archived-global','archived-host','bad-path','duplicate','missing-skill','invalid-hive'])cases.push({enabled:true,global:true,host:true,mode});
  for(const [i,test] of cases.entries()){
    const base=join(root,'prepare-'+i),home=join(base,'home'),target=join(base,'skills');await mkdir(home,{recursive:true});
    const defaults=document(null,test.enabled),global=test.global==='missing'?null:document(null,test.global,'global'),host=test.host==='missing'?null:document(hostId,test.host,'host');
    if(test.mode==='global-scope')global.hostId=hostId;
    if(test.mode==='host-scope')host.hostId=null;
    if(test.mode==='host-schema')host.files[0].content=42;
    if(test.mode==='bad-path')host.files[0].path='../SKILL.md';
    if(test.mode==='duplicate')host.files.push({...host.files[0]});
    if(test.mode==='missing-skill')host.files.shift();
    const effectiveHive=test.mode==='invalid-hive'?'invalid':hive,replies=[];
    const connection={async request(method,params){
      const scope=method==='objects.stat'?(params.path==='/'+original.environmentName('skills',null)?'global':'host'):params.objectId;
      const doc=scope==='global'?global:host;let result,error;
      if(test.mode===scope+'-'+(method==='objects.stat'?'stat':'read')+'-fail')error={code:'service_unavailable',outcome:'unknown'};
      else if(method==='objects.stat'){
        if(!doc)error={code:'not_found',outcome:'not_executed'};
        else result={id:scope,currentRevision:scope==='global'?7:11,effectivelyArchived:test.mode==='archived-'+scope};
      }else{
        assert.equal(method,'objects.read');
        result={object:{id:scope,contractKey:test.mode==='global-contract'&&scope==='global'?'other':'agent/skills'},revision:{revision:99,contractVersion:test.mode==='host-version'&&scope==='host'?'2.0.0':'1.0.0',contentHash:hashJson(doc)},content:{encoding:'json',value:doc}};
      }
      replies.push({method,params,...(error?{error}:{result})});
      if(error)throw new IvyError(error.code,'fixture failure',error.outcome);return result;
    }};
    const manager=new original.SkillsManager(home,target,hostId,effectiveHive,defaults);await manager.synchronize(connection);await manager.close();
    let expected;
    if(manager.status.code)expected={error:manager.status.code};
    else{
      const state=JSON.parse(await readFile(join(base,'.ivy-agent-skills-'+digest(target).slice(7,23),'state.json'),'utf8'));
      const selected=host&&host.enabled!==null&&test.mode!=='archived-host'?host:global&&global.enabled!==null&&test.mode!=='archived-global'?global:defaults;
      expected={result:{groups:[...original.normalizeSkills(selected)].map(([name,files])=>({name,files:[...files].map(([name,bytes])=>[name,digest(bytes)])})),desiredVersion:manager.status.desiredVersion,authority:state.authority}};
    }
    preparation.push({mode:test.mode??'inheritance',defaults,hostId,hive:effectiveHive,replies,expected});
  }
  // Keep actual ownership/migration effects; malformed typed state is checked
  // directly by Rust instead of requiring JavaScript primitive boxing.
  const modes=['missing','null','array-state','object-state','true-state','wrong-version','wrong-authority','v2-empty','v1-empty','owned-same','owned-change','owned-edit','owned-missing','owned-number','unowned','legacy-same','legacy-change','legacy-edit','legacy-target','legacy-nonprefix','invalid-name','invalid-removed','retained-unselected','applied-array','applied-string','applied-null'];
  const names=['alpha','beta','ivy-managed-alpha','removed'];
  for(const selection of ['one','two','empty','disabled'])for(const mode of modes){
    const base=join(root,'ownership-'+ownership.length),home=join(base,'home'),target=join(base,'skills'),stage=join(base,'desired');await mkdir(home,{recursive:true});await mkdir(target);
    const defaults=document(null,selection!=='disabled');if(selection==='empty')defaults.files=[];if(selection==='two')defaults.files.push(file('beta/SKILL.md','second'));
    const groups=original.normalizeSkills(defaults),after=[];
    for(const [name,files]of groups){await writeTree(join(stage,name),files);after.push([name,await original.treeHash(join(stage,name))]);}
    const desiredVersion=original.selectEnvironmentDocument(defaults,null,null).version;
    let state;
    if(mode==='null')state=null;else if(mode==='array-state')state=[];else if(mode==='object-state')state={};else if(mode==='true-state')state=true;
    else if(mode!=='missing')state={schemaVersion:mode.startsWith('legacy-')||mode==='v1-empty'?1:2,authority,applied:{},appliedVersion:'prior'};
    if(mode==='wrong-version')state.schemaVersion=3;
    if(mode==='wrong-authority')state.authority='other';
    if(mode.startsWith('owned-')||mode.startsWith('legacy-')&&mode!=='legacy-nonprefix'){
      const name=mode.startsWith('legacy-')?'ivy-managed-alpha':'alpha';
      await writeTree(join(target,name),new Map([['SKILL.md',mode.endsWith('same')?'default\né':'old'],['nested/readme.txt','contents']]));
      state.applied[name]=await original.treeHash(join(target,name));
      if(mode.endsWith('edit'))await writeFile(join(target,name,'SKILL.md'),'local edit');
      if(mode==='owned-missing')await rm(join(target,name),{recursive:true});
      if(mode==='owned-number')state.applied[name]=42;
    }
    if(mode==='unowned'||mode==='legacy-target')await writeTree(join(target,'alpha'),new Map([['SKILL.md','outside management']]));
    if(mode==='legacy-nonprefix')state.applied.alpha='bad';
    if(mode==='invalid-name')state.applied['../outside']='bad';
    if(mode==='invalid-removed')state.applied['UPPER']='bad';
    if(mode==='retained-unselected'){await writeTree(join(target,'removed'),new Map([['SKILL.md','locally changed']]));state.applied.removed='old hash';}
    if(mode==='applied-array')state.applied=['bad'];if(mode==='applied-string')state.applied='bad';if(mode==='applied-null')state.applied=null;
    const stateDir=join(base,'.ivy-agent-skills-'+digest(target).slice(7,23)),statePath=join(stateDir,'state.json');
    if(state!==undefined){await mkdir(stateDir);await writeFile(statePath,JSON.stringify(state));}
    const beforeTrees=await snapshot(target,names);
    const manager=new original.SkillsManager(home,target,hostId,hive,defaults);await manager.synchronize(missing);await manager.close();
    const afterTrees=await snapshot(target,names),stored=await readFile(statePath,'utf8').then(JSON.parse,e=>{if(e.code==='ENOENT')return null;throw e;});
    const expected=manager.status.code?{error:manager.status.code}:{result:{state:stored,trees:afterTrees}};
    if(manager.status.code)assert.deepEqual(afterTrees,beforeTrees,'conflict must precede replacement: '+mode);
    ownership.push({mode,selection,state:state??null,authority,desiredVersion,after,beforeTrees,expected});
  }
}finally{await rm(root,{recursive:true,force:true});}
assert.equal(preparation.length,47);assert.equal(ownership.length,104);
process.stdout.write(JSON.stringify({preparation,ownership}));
