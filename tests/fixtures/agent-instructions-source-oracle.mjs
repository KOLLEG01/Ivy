import {registerHooks} from 'node:module';
import {readFileSync} from 'node:fs';
import {mkdtemp,mkdir,writeFile,readFile,rm,readdir,lstat} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {hashJson,digest} from '../../dist/packages/contracts/src/canonical.js';
import {composeHiveMcpInstructions,compactHiveMcpInstructions,hiveMcpInstructions,hiveMcpAgentSummary} from '../../dist/instructions/hive-mcp.js';
const hook=registerHooks({load(url,context,next){
  if(url.endsWith('/dist/services/agent-manager/src/instructions.js'))return {format:'module',source:readFileSync(new URL(url),'utf8')+'\nexport {discoverMcpInstructions};\n',shortCircuit:true};
  return next(url,context);
}});
const original=await import('../../dist/services/agent-manager/src/instructions.js');hook.deregister();
const answer=async(fn)=>{try{return {result:await fn()};}catch(e){return {error:IvyError.from(e).toWire()};}};
const failure=(code='service_unavailable',outcome='unknown')=>({code,message:'fixture failure',outcome});
const throwFailure=e=>{throw new IvyError(e.code,e.message,e.outcome);};
const document=(hostId=null,enabled=true,includeLocal=true,text='Default fixture')=>({schemaVersion:1,hostId,enabled,includeLocal,text});
const names=[null,'','fixture','Host','host 😀','\ud800','\udc00','\ufffd','e\u0301','é','a\0b'].map(hostId=>({hostId,expected:original.instructionsName(hostId)}));
const reads=[];
for(const hostId of [null,'fixture'])for(const mode of ['valid','missing','archived','null-stat','false-stat','zero-stat','empty-stat','stat-error','read-missing','read-unknown','contract','version','encoding','scope','schema','inherit','archived-truthy']){
  const stat={id:'object',currentRevision:7,effectivelyArchived:mode==='archived'?true:mode==='archived-truthy'?'yes':false};
  const value={object:{contractKey:mode==='contract'?'other':'agent/instructions'},revision:{contractVersion:mode==='version'?'2.0.0':'1.0.0',contentHash:'sha256:'+'a'.repeat(64)},content:{encoding:mode==='encoding'?'base64':'json',value:document(mode==='scope'?(hostId===null?'fixture':null):hostId,mode==='inherit'?null:true)}};
  if(mode==='schema')value.content.value.includeLocal='true';
  const input={hostId,mode,stat:mode==='null-stat'?null:mode==='false-stat'?false:mode==='zero-stat'?0:mode==='empty-stat'?'':stat,value,statError:mode==='missing'?failure('not_found','not_executed'):mode==='stat-error'?failure():null,readError:mode==='read-missing'?failure('not_found','not_executed'):mode==='read-unknown'?failure('outcome_unknown'):null};
  const calls=[];const client={async request(method,params){calls.push({method,params});const e=method==='objects.stat'?input.statError:input.readError;if(e)throwFailure(e);return method==='objects.stat'?input.stat:value;}};
  reads.push({...input,calls,expected:await answer(()=>original.readInstructions(client,hostId))});
}
const cap='\n\nServices registered to this Hive provide the following capabilities:\n',workflow='\n\nTool workflows:\n';
const compact=[];
for(const text of ['',hiveMcpInstructions,'plain guidance',cap,cap+'- one: hint',workflow+'ignored'+cap+'- one: hint',cap+'- one: hint'+workflow+'hidden',cap+'first'+cap+'second'+workflow+'hidden','prefix😀'+cap+'\ud800'+workflow+'x','\ud800'+cap+'tail\udc00',cap+'first\r\n\r\nTool workflows:\r\nnot exact',cap+'x\0y'])compact.push({text,expected:compactHiveMcpInstructions(text)});
const services=['a','A','á','a\u0301','z','ä','Z','10','2','_','😀','\ud800','\udc00','é','e\u0301'];
const hints=['First','first','ﬁrst','ångström','Ångström','é','e\u0301','e',' e ','\tHint\uFEFF','Hint','\u0085','\u200B','😀','\ud800','\udc00','',null,undefined];
const legacy=[];let seed=12345;
const random=n=>{seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed%n;};
for(let i=0;i<96;i++){
  const entries=Array.from({length:random(16)},()=>({serviceName:services[random(services.length)],discoveryHint:hints[random(hints.length)]}));
  if(i%4===0&&entries.length)entries.push({...entries[0]});
  legacy.push({hints:entries,expected:await answer(()=>compactHiveMcpInstructions(composeHiveMcpInstructions(entries)))});
}
const prefixBytes=Buffer.byteLength(hiveMcpInstructions+cap+'- one: '),budget=32768-prefixBytes;
assert(budget>0);
for(const count of [budget,budget+1]){const entries=[{serviceName:'one',discoveryHint:'x'.repeat(count)}];legacy.push({hints:entries,expected:await answer(()=>compactHiveMcpInstructions(composeHiveMcpInstructions(entries)))});}
const values=[...new Set([...services,...hints.map(s=>s?.trim()),...legacy.flatMap(row=>row.hints.flatMap(e=>[e.serviceName,e.discoveryHint?.trim()]))].filter(s=>typeof s==='string'&&s.length>0&&s.length<512))];
const collation={locale:'en',values,matrix:values.map(left=>values.map(right=>Math.sign(left.localeCompare(right,'en')))),scope:'Replay table for owned projection tests only; not a production comparator implementation.'};
const discovery=[];
async function discoveryCase(name,modern,pages){
  const replies=[],calls=[];let page=0;
  const client={async request(method,params){
    calls.push({method,params});const response=method==='discovery.instructions'?modern:pages[Math.min(page++,pages.length-1)];
    assert(response,'Unplanned fixture call');replies.push({method,params,...response});if(response.error)throwFailure(response.error);return response.result;
  }};
  const expected=await answer(()=>original.discoverMcpInstructions(client));discovery.push({name,replies,calls,expected});
}
const absent={error:failure('not_found','not_executed')};
for(const row of compact)await discoveryCase('modern-'+discovery.length,{result:{instructions:row.text}},[]);
await discoveryCase('modern-failure',{error:failure()},[]);
await discoveryCase('modern-malformed',{result:{}},[]);
await discoveryCase('legacy-absent',absent,[absent]);
await discoveryCase('legacy-failure',absent,[{error:failure('outcome_unknown')}]);
for(const result of [null,false,0,''])await discoveryCase('legacy-falsy-'+typeof result,absent,[{result}]);
await discoveryCase('legacy-empty',absent,[{result:{items:[],nextCursor:null}}]);
const item=(serviceName,description)=>({kind:'service',serviceName,description});
await discoveryCase('legacy-two-pages',absent,[{result:{items:[item('é','Hint'),{kind:'object',name:'ignore'},item('a','First')],nextCursor:'next'}},{result:{items:[item('e\u0301','Hint'),item('a','First'),item('A','first')],nextCursor:null}}]);
await discoveryCase('legacy-repeated-cursor-bound',absent,[{result:{items:[],nextCursor:'same'}}]);
await discoveryCase('legacy-exactly-twenty',absent,Array.from({length:20},(_,i)=>({result:{items:[],nextCursor:i===19?null:String(i)}})));
await discoveryCase('legacy-disappears-after-page',absent,[{result:{items:[item('a','First')],nextCursor:'next'}},absent]);
await discoveryCase('legacy-full-budget',absent,[{result:{items:[item('one','x'.repeat(budget+1))],nextCursor:null}}]);

const root=await mkdtemp(join(tmpdir(),'ivy-instructions-source-')),managers=[];
async function inputFiles(home){
  const result={};
  async function walk(path,relative=''){
    for(const name of (await readdir(path)).sort()){
      if(name.startsWith('executor-lock.sqlite'))continue;
      const full=join(path,name),key=relative+name,info=await lstat(full);
      if(info.isDirectory()){result[key]={directory:true};await walk(full,key+'/');}
      else result[key]={hex:(await readFile(full)).toString('hex')};
    }
  }
  await walk(home);return result;
}
const fileHash=async path=>readFile(path).then(digest,e=>{if(e.code==='ENOENT')return null;throw e;});
try{
  const scenarios=[];
  for(const enabled of [true,false])for(const global of ['missing',null,false,true])for(const host of ['missing',null,false,true])scenarios.push({enabled,global,host});
  for(const mode of ['global-stat-error','global-read-error','host-stat-error','host-read-error','global-scope','host-contract','host-schema','archived-host','guidance-failure','legacy-hints','excluded-local','local-invalid','prior-bootstrap','fresh-bootstrap'])scenarios.push({enabled:true,global:true,host:true,mode});
  for(const scenario of scenarios){
    const home=join(root,String(managers.length));await mkdir(home);
    const mode=scenario.mode??'inheritance',defaults=document(null,scenario.enabled,mode!=='excluded-local');
    const global=scenario.global==='missing'?null:document(null,scenario.global,true,'Global fixture');
    const host=scenario.host==='missing'?null:document('fixture',scenario.host,mode!=='excluded-local','Host fixture');
    if(mode==='global-scope')global.hostId='fixture';if(mode==='host-schema')host.includeLocal=42;
    if(mode==='excluded-local')await mkdir(join(home,'AGENTS.md'));else await writeFile(join(home,'AGENTS.md'),mode==='local-invalid'?Buffer.from([255]):'Local fixture');
    await writeFile(join(home,'AGENTS.override.md'),'Original user override\r\n');
    const make=()=>new original.InstructionsManager(home,'fixture','https://fixture.example',defaults);
    function connection(replies){return {async request(method,params){
      let result,error;
      if(method==='objects.stat'||method==='objects.read'){
        const scope=method==='objects.stat'?(params.path==='/'+original.instructionsName(null)?'global':'host'):params.objectId,doc=scope==='global'?global:host;
        if(mode===scope+'-'+(method==='objects.stat'?'stat':'read')+'-error')error=failure();
        else if(!doc)error=failure('not_found','not_executed');
        else if(method==='objects.stat')result={id:scope,currentRevision:scope==='global'?7:11,effectivelyArchived:mode==='archived-'+scope};
        else result={object:{contractKey:mode==='host-contract'&&scope==='host'?'other':'agent/instructions'},revision:{contractVersion:'1.0.0',contentHash:hashJson(doc)},content:{encoding:'json',value:doc}};
      }else if(method==='discovery.instructions'){
        if(mode==='guidance-failure')error=failure();else if(mode==='legacy-hints')error=failure('not_found','not_executed');else result={instructions:hiveMcpInstructions+cap+'- example: Fixture capability.'+workflow+'Hidden workflow.'};
      }else{assert.equal(method,'discovery.list');result={items:[item('a','First'),item('A','first'),item('é','Hint'),item('e\u0301','Hint')],nextCursor:null};}
      replies.push({method,params,...(error?{error}:{result})});if(error)throwFailure(error);return result;
    }};}
    if(mode==='prior-bootstrap'){const seeded=make();await seeded.synchronize(connection([]));assert.equal(seeded.status.state,'applied');}
    const files=await inputFiles(home),manager=make(),replies=[];
    if(mode.endsWith('bootstrap'))await manager.bootstrap();else await manager.synchronize(connection(replies));
    await manager.close();const {observedAt,...status}=manager.status;assert(/Z$/.test(observedAt));status.home='.';
    managers.push({mode,defaults,files,bootstrap:mode.endsWith('bootstrap'),replies,expected:{status,target:await fileHash(join(home,'AGENTS.override.md')),state:await fileHash(join(home,'.ivy-agent-instructions/state.json'))}});
  }
}finally{await rm(root,{recursive:true,force:true});}
process.stdout.write(JSON.stringify({names,reads,compact,legacy,collation,discovery,managers,summary:hiveMcpAgentSummary,fullInstructionsBytes:Buffer.byteLength(hiveMcpInstructions)}));
