import {readFileSync,existsSync} from 'node:fs';
import {mkdtemp,mkdir,writeFile,readFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname,relative} from 'node:path';
import assert from 'node:assert/strict';
import {InstructionsManager} from '../../dist/services/agent-manager/src/instructions.js';
import {McpConfigurationManager,SkillsManager} from '../../dist/services/agent-manager/src/environment.js';
import {ClaudeMcpConfigurationManager} from '../../dist/services/agent-manager/src/claude-mcp.js';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
import {digest,canonical} from '../../dist/packages/contracts/src/canonical.js';

// Execute exact original composition spans with its real file managers; do not
// start a native provider, service socket, reset, journal or capability store.
const source=readFileSync('services/agent-manager/src/main.ts','utf8').replaceAll('\r\n','\n');
function span(begin,end){const first=source.indexOf(begin),last=source.indexOf(end,first+begin.length);assert(first>=0&&last>first,begin);return source.slice(first,last);}
const factories=span('  const managedSkillsRoot =','  journal.onTerminal');
const executableFactories=factories.replaceAll(': string','').replaceAll(': Agent.McpDocument','').replaceAll('config.credential!','config.credential');
let before=span('      beforeLaunch: async (home) => {','      beginEpoch:');
before=before.slice(before.indexOf('{')+1,before.lastIndexOf('      },'));
const after=span('    if (\n      settings.patcherEnabled','    for (const input of startupClocks');
const status=span('      ...(instructions ?','      ...(owner.nativeHome');
const refresh=span('        if (instructions && mcp && skills) {','        void refreshThreads');
const close=span('          await Promise.all([\n            instructions?.close(),','          locations?.close();');
const factory=new Function('settings','config','defaults','bootstrap','InstructionsManager','McpConfigurationManager','ClaudeMcpConfigurationManager','SkillsManager','join','dirname','requireThat',`
  let instructions=null,mcp=null,skills=null,environmentAt=0;
  ${executableFactories}
  return {before:async home=>{${before}},after:owner=>{${after}},
    refresh:async connection=>{${refresh}},status:()=>({${status}}),
    at:()=>environmentAt,close:async()=>{${close}}};
`);
const root=await mkdtemp(join(tmpdir(),'ivy-environment-owner-'));
const definitions=[
 {mode:'owned',before:true},{mode:'owned-claude',before:true,claude:true},{mode:'explicit-skills',before:true,explicit:true},
 {mode:'disabled',before:true,disabled:true},{mode:'fallback',before:false},{mode:'fallback-refreshed',before:false,refresh:true},
 {mode:'fallback-no-home',before:false,noHome:true},{mode:'owned-unavailable',before:true,unavailable:true},
 {mode:'claude-unavailable',before:true,claude:true,unavailable:true},{mode:'owned-conflict',before:true,conflict:true},
 {mode:'claude-conflict',before:true,claude:true,conflict:true},{mode:'owned-after-other-home',before:true,otherHome:true},
 {mode:'fallback-empty-home',before:false,emptyHome:true},
];
const defaults=conflict=>({instructions:{schemaVersion:1,hostId:null,enabled:true,includeLocal:true,text:'Fixture instructions'},
 mcp:{schemaVersion:1,hostId:null,enabled:true,servers:[{name:'fixture',url:conflict?'https://other.example/mcp':'https://fixture.example/mcp',enabled:true,authentication:'agent-manager',startupTimeoutSeconds:10,toolTimeoutSeconds:30}]},
 skills:{schemaVersion:1,hostId:null,enabled:true,files:[{path:'fixture/SKILL.md',content:'# Fixture\n'}]}});
const file=async path=>{try{const bytes=await readFile(path);return{hash:digest(bytes),bytes:bytes.length,text:bytes.length<=8192?bytes.toString('utf8'):null};}catch(error){if(error.code==='ENOENT')return null;throw error;}};
function normalized(value,base){return Object.fromEntries(Object.entries(value).filter(([name])=>name!=='observedAt').map(([name,v])=>[name,['home','target'].includes(name)&&v!==null?relative(base,v).replaceAll('\\','/'):v]));}
async function snapshot(api,base){const original=api.status(),status={};let files=null;
 if(original.instructions){status.instructions=normalized(original.instructions,base);status.environment={mcp:normalized(original.environment.mcp,base),skills:normalized(original.environment.skills,base)};
  files={instructions:await file(join(original.instructions.home,'AGENTS.override.md')),mcp:await file(original.environment.mcp.target),skill:await file(join(original.environment.skills.target,'fixture/SKILL.md'))};}
 return{status,files};
}
async function setup(definition,label=definition.mode){
 const base=join(root,label),home=join(base,'home'),data=join(base,'data');await mkdir(home,{recursive:true});await mkdir(data);await mkdir(join(base,'other-home'));await writeFile(join(home,'AGENTS.md'),'# Local\nFixture local text\n');
 const settings={...(definition.disabled?{patcherEnabled:false}:{}),...(definition.claude?{appServer:{mode:'claude-adapter'}}:{}),...(definition.explicit?{skillsRoot:join(base,'explicit-skills')}:{})};
 const config={dataRoot:data,hostId:'fixture',publicBaseUrl:'https://fixture.example',credential:'fixture-only'};
 const skill=settings.skillsRoot??(definition.claude?join(data,'claude-config','skills'):join(base,'.agents','skills'));
 const mcp=definition.claude?join(data,'claude-mcp.json'):join(home,'config.toml');
 const calls=[];let first=null,gate=null,entered=null;
 const client={async request(method,params){calls.push({method,params});if(first===null)first={instructions:existsSync(join(home,'AGENTS.override.md')),mcp:existsSync(mcp),skill:existsSync(join(skill,'fixture/SKILL.md'))};
  entered?.();if(gate)await gate;if(definition.unavailable)throw new IvyError('service_unavailable','fixture unavailable','unknown');
  if(method==='openai.getConfiguration')return{revision:0,configuration:{schemaVersion:1,appId:null}};
  if(method==='discovery.instructions')return{instructions:'Fixture guidance'};
  assert.equal(method,'objects.stat');throw new IvyError('not_found','fixture missing');}};
 const api=factory(settings,config,defaults(definition.conflict),client,InstructionsManager,McpConfigurationManager,ClaudeMcpConfigurationManager,SkillsManager,join,dirname,requireThat);
 return{base,home,api,calls,first:()=>first,client,block(held,started){gate=held;entered=started;}};
}
try{
 const rows=[];
 for(const definition of definitions){const run=await setup(definition);let error=null;
  if(definition.before)try{await run.api.before(run.home);}catch(e){error=IvyError.from(e).code;}
  const owner=definition.noHome?{}:{nativeHome:definition.emptyHome?'':definition.otherHome?join(run.base,'other-home'):run.home};run.api.after(owner);
  const startupRecorded=run.api.at()>0;if(definition.refresh)await run.api.refresh(run.client);
  await run.api.close();rows.push({...definition,expected:{error,startupRecorded,firstLive:run.first(),calls:run.calls.sort((a,b)=>canonical(a)<canonical(b)?-1:canonical(a)>canonical(b)?1:0),...await snapshot(run.api,run.base)}});
 }
 const abandoned=await setup({mode:'abandoned',before:true});let release,entered;const held=new Promise(resolve=>{release=resolve;}),started=new Promise(resolve=>{entered=resolve;});abandoned.block(held,entered);
 const accepted=abandoned.api.before(abandoned.home);await started;let closed=false;const close=abandoned.api.close().then(()=>{closed=true;});await Promise.resolve();const closedWhileBlocked=closed;release();await Promise.all([accepted,close]);
 process.stdout.write(JSON.stringify({source:{sha256:digest(source),spans:{factories:factories.length,before:before.length,after:after.length,status:status.length,refresh:refresh.length,close:close.length}},rows,abandoned:{closedWhileBlocked,startupRecorded:abandoned.api.at()>0,...await snapshot(abandoned.api,abandoned.base)}}));
}finally{await rm(root,{recursive:true,force:true});}
