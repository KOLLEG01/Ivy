import {mkdtemp,mkdir,writeFile,readFile,rm,readdir,lstat,symlink} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,relative} from 'node:path';
import assert from 'node:assert/strict';
import {InstructionsManager,instructionsName} from '../../dist/services/agent-manager/src/instructions.js';
import {hiveMcpAgentSummary} from '../../dist/instructions/hive-mcp.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest,hashJson} from '../../dist/packages/contracts/src/canonical.js';

const stateDir='.ivy-agent-instructions', stateFile=stateDir+'/state.json';
const defaults={schemaVersion:1,hostId:null,enabled:true,includeLocal:true,text:'Project fixture 😀'};
const source=(changes={})=>({defaults:{...defaults,...changes},global:null,host:null});
const disabled=source({enabled:false});
const changed=source({text:'Changed fixture'});
const envelope=(document)=>({document,objectId:instructionsName(document.hostId),revision:7,contentHash:hashJson(document)});
const scoped={...source(),global:envelope({...defaults,text:'Hive fixture'}),host:envelope({...defaults,hostId:'fixture',text:'Host fixture'})};
const root=await mkdtemp(join(tmpdir(),'ivy-instructions-files-'));
const rows=[];
const manager=(home)=>new InstructionsManager(home,'fixture','https://fixture.example',defaults);
function client(m,sequence){
  let reads=0,selected;
  return {get reads(){return reads;},async request(method,params){
    if(method==='objects.stat'&&params.path==='/'+instructionsName(null)){
      selected=sequence[Math.min(reads++,sequence.length-1)];m.defaults=selected.defaults;
      if(selected.error)throw new IvyError(selected.error,'Fixture source unavailable.');
    }
    if(method==='discovery.instructions')return {instructions:'Unused full guidance without a capabilities section.'};
    assert(method==='objects.stat'||method==='objects.read');
    const key=String(params.path??params.objectId).replace(/^\//,''),value=key===instructionsName(null)?selected.global:selected.host;
    if(!value)throw new IvyError('not_found','Missing fixture source.');
    if(method==='objects.stat')return {id:value.objectId,currentRevision:value.revision,effectivelyArchived:false};
    return {object:{contractKey:'agent/instructions'},revision:{contractVersion:'1.0.0',contentHash:value.contentHash},content:{encoding:'json',value:value.document}};
  }};
}
async function list(home,inputs=false){
  const result={};
  async function walk(path){
    for(const name of (await readdir(path)).sort()){
      if(name.startsWith('executor-lock.sqlite'))continue;
      const full=join(path,name),key=relative(home,full).replaceAll('\\','/'),info=await lstat(full);
      if(info.isSymbolicLink())result[key]={kind:'link'};
      else if(info.isDirectory()){result[key]={kind:'directory'};await walk(full);}
      else {const bytes=await readFile(full);result[key]=inputs
        ? bytes.length>65536&&bytes.every(b=>b===bytes[0])?{kind:'repeat',byte:bytes[0],size:bytes.length}:{kind:'file',hex:bytes.toString('hex')}
        : {kind:'file',size:bytes.length,hash:digest(bytes)};}
    }
  }
  await walk(home);return result;
}
function status(m,home){
  const {observedAt,...s}=m.status;assert(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(observedAt));
  return {...s,home:relative(home,s.home).replaceAll('\\','/')||'.'};
}
async function run(name,setup=async()=>{},steps=[{sources:[source()]}]){
  const home=join(root,String(rows.length));await mkdir(home);
  const file=async(path,bytes)=>{await mkdir(join(home,path,'..'),{recursive:true});await writeFile(join(home,path),bytes);};
  const seed=async()=>{const m=manager(home),c=client(m,[source()]);await m.synchronize(c);assert.equal(m.status.state,'applied');};
  const state=async(change)=>{const value=JSON.parse(await readFile(join(home,stateFile),'utf8'));await file(stateFile,JSON.stringify(change(value)??value));};
  await setup({home,file,seed,state});
  const files=await list(home,true),m=manager(home),initial=status(m,home),outputs=[];
  for(const step of steps){
    for(const [path,bytes]of Object.entries(step.files??{}))await file(path,Buffer.from(bytes,'hex'));
    const c=client(m,step.sources);await m.synchronize(c);await m.close();
    outputs.push({status:status(m,home),files:await list(home),reads:c.reads});
  }
  rows.push({name,files,steps,initial,outputs});
}
try{
  await run('fresh');
  await run('original-and-local',async({file})=>{await file('AGENTS.override.md','Original override — Grüße\r\n');await file('AGENTS.md','Local fixture');},[{sources:[scoped]},{sources:[scoped]},{sources:[disabled]},{sources:[source()]}]);
  await run('empty-original',({file})=>file('AGENTS.override.md',''));
  await run('binary-original-restored',({file})=>file('AGENTS.override.md',Buffer.from([255,0,128])),[{sources:[source()]},{sources:[disabled]}]);
  await run('bom-local',({file})=>file('AGENTS.md','\uFEFFLocal BOM'));
  await run('disabled-no-effects',async({file})=>{await file('AGENTS.override.md','User original');},[{sources:[disabled]}]);
  await run('disabled-existing-root',({home})=>mkdir(join(home,stateDir)),[{sources:[disabled]}]);
  await run('excluded-local-directory',({home})=>mkdir(join(home,'AGENTS.md')),[{sources:[source({includeLocal:false})]}]);
  await run('included-local-directory',({home})=>mkdir(join(home,'AGENTS.md')));
  await run('invalid-local-utf8',({file})=>file('AGENTS.md',Buffer.from([255])));
  await run('oversized-local',({file})=>file('AGENTS.md',Buffer.alloc(1024*1024+1,120)));
  await run('composed-size-limit',({file})=>file('AGENTS.md',Buffer.alloc(32768,120)));
  await run('target-directory',({home})=>mkdir(join(home,'AGENTS.override.md')));
  await run('oversized-target',({file})=>file('AGENTS.override.md',Buffer.alloc(1024*1024+1,120)));
  for(const [name,raw]of [['empty',''],['invalid','{'],['bom','\uFEFFnull'],['utf8',Buffer.from([255])],['null','null'],['false','false'],['zero','0'],['empty-string','""'],['array','[]'],['object','{}']])
    await run('state-'+name,({file})=>file(stateFile,raw));
  await run('state-directory',async({home})=>{await mkdir(join(home,stateDir));await mkdir(join(home,stateFile));});
  await run('oversized-state',({file})=>file(stateFile,Buffer.alloc(1024*1024+1,32)));
  await run('state-root-file',({file})=>file(stateDir,'x'));
  for(const suffix of ['-journal','-wal','-shm'])
    await run('lock-companion-'+suffix,async({home})=>{await mkdir(join(home,stateDir));await mkdir(join(home,stateDir,'executor-lock.sqlite'+suffix));});
  await run('changed-managed',async({file,seed})=>{await file('AGENTS.override.md','Before Ivy');await seed();},[{sources:[changed]},{sources:[disabled]}]);
  await run('foreign-authority',async({seed,state})=>{await seed();await state(s=>{s.authority=digest('other');});});
  await run('future-schema',async({seed,state})=>{await seed();await state(s=>{s.schemaVersion=2;});});
  await run('extra-state-fields',async({seed,state})=>{await seed();await state(s=>{s.extra={opaque:'retained',n:7};});},[{sources:[changed]}]);
  for(const disabledNow of [false,true])
    await run('local-edit-'+disabledNow,async({seed,file})=>{await seed();await file('AGENTS.override.md','User edit');},[{sources:[disabledNow?disabled:changed]}]);
  await run('deleted-managed',async({seed,home})=>{await seed();await rm(join(home,'AGENTS.override.md'));});
  await run('disable-removes',async({seed})=>seed(),[{sources:[disabled]}]);
  for(const phase of ['before','after','third','missing-blob','bad-blob','blob-directory','invalid-reference','after-without-blob']){
    await run('pending-write-'+phase,async({file,seed,state,home})=>{
      await file('AGENTS.override.md','Original');await seed();
      await state(asyncState=>{asyncState.pending={before:asyncState.originalHash,after:asyncState.appliedHash,version:asyncState.appliedVersion,active:true};asyncState.appliedHash=asyncState.originalHash;asyncState.appliedVersion=null;});
      const s=JSON.parse(await readFile(join(home,stateFile),'utf8'));
      if(!phase.startsWith('after'))await file('AGENTS.override.md',phase==='third'?'Third-party edit':'Original');
      const blob=stateDir+'/'+s.pending.after.slice(7)+'.md';
      if(['missing-blob','after-without-blob','blob-directory'].includes(phase))await rm(join(home,blob));
      if(phase==='bad-blob')await file(blob,'Changed backup');
      if(phase==='blob-directory')await mkdir(join(home,blob));
      if(phase==='invalid-reference')await state(s=>{s.pending.after='../../invalid';});
    });
  }
  for(const phase of ['before','after','third']){
    await run('pending-delete-'+phase,async({seed,state,home,file})=>{
      await seed();await state(s=>{s.pending={before:s.appliedHash,after:null,version:'fixture-pending',active:false};});
      if(phase==='after')await rm(join(home,'AGENTS.override.md'));
      if(phase==='third')await file('AGENTS.override.md','Third-party edit');
    },[{sources:[disabled]}]);
  }
  await run('read-error-first',async()=>{},[{sources:[{...source(),error:'service_unavailable'}]}]);
  await run('read-error-under-lock',async()=>{},[{sources:[source(),{...source(),error:'connection_lost'}]}]);
  await run('fresh-locked-read-wins',async()=>{},[{sources:[source(),changed]}]);
  await run('retained-status',async()=>{},[{sources:[source()]},{sources:[{...source(),error:'connection_lost'}]},{sources:[source()],files:{'AGENTS.override.md':Buffer.from('User edit').toString('hex')}},{sources:[disabled]}]);
  // Junctions are available without developer-mode symlink privileges on Windows.
  const linked=[];
  for(const kind of ['home','root','target','local']){
    const base=join(root,'linked-'+kind),actual=join(base,'home'),outside=join(base,'outside');await mkdir(actual,{recursive:true});await mkdir(outside);
    const link=kind==='home'?join(base,'link'):join(actual,kind==='root'?stateDir:kind==='target'?'AGENTS.override.md':'AGENTS.md');
    await symlink(kind==='home'?actual:outside,link,process.platform==='win32'?'junction':'dir');
    const m=manager(kind==='home'?link:actual),c=client(m,[source()]);await m.synchronize(c);
    linked.push({kind,status:status(m,actual),reads:c.reads,files:await list(actual),outside:await list(outside)});
  }
  process.stdout.write(JSON.stringify({guidance:hiveMcpAgentSummary,rows,linked}));
}finally{await rm(root,{recursive:true,force:true});}
