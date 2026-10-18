import {existsSync,readFileSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
const contracts=new URL('../../../packages/contracts/src/',import.meta.url);
const data=source=>'data:text/javascript;base64,'+Buffer.from(source).toString('base64');
const sdk=data([
  `export {canonical,hashJson} from '${new URL('canonical.ts',contracts)}';`,
  `export {IvyError,requireThat} from '${new URL('errors.ts',contracts)}';`,
  `export {validateAgent} from '${new URL('agent-validation.ts',contracts)}';`,
  `export {validateChat} from '${new URL('chat-validation.ts',contracts)}';`,
].join('\n'));
// Load the original driver methods without constructing its unrelated durable
// store/evidence graph. Every excluded entrypoint throws if accidentally used.
const excluded={
  './store.js':['ChatStore','mutation'], './operations.js':['contention'],
  './native-plan.js':['resolveChatPlan','validateChatNativeDraft'],
  './native-evidence.js':['ChatNativeEvidence','verifyChatNativeRequest'],
};
registerHooks({resolve(s,c,n){
  if(s==='../../../packages/sdk/src/node.js')return {url:sdk,shortCircuit:true};
  if(c.parentURL?.endsWith('/services/chat-bridge/src/native-driver.ts')&&excluded[s]){
    const source=excluded[s].map(name=>`export function ${name}(){throw Error('Outside owner/readiness scope: ${name}');}`).join('\n')+
      (s==='./native-evidence.js'?`\nexport {checkedNativeContract as chatNativeCatalog} from '${new URL('checked-native-contract.ts',contracts)}';`:'');
    return {url:data(source),shortCircuit:true};
  }
  if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}
  return n(s,c);
},load(u,c,n){
  if(u.endsWith('.ts'))return {format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(u),'utf8'),{mode:'transform'}),shortCircuit:true};
  return n(u,c);
}});
const {ChatNativeDriver}=await import('../../../services/chat-bridge/src/native-driver.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {checkedNativeContract}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,
 project:{serviceNodeId:'native-agent',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},
 nativePlan:unpersistedNativePlan('0.159.2'),channels:[]};
const source=checkedNativeContract(definition.nativePlan.nativeVersion,{sourceHash:definition.nativePlan.catalogSourceHash});
const owner={serviceNodeId:'chat-provider',generation:7};
const node={principalId:'chat-service',serviceName:'chat-bridge',connected:true,synced:true};
const status={serviceNodeId:'native-agent',hostId:'native-host',nativeVersion:source.catalog.version,
 nativeExecutableHash:source.catalog.nativeExecutableHash,catalogHash:source.catalogHash,epoch:'epoch',pid:42,state:'ready',
 observedAt:'2026-01-02T03:04:05.006Z',code:null,initialized:{},pendingInputs:0,
 operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const cases=[];
function add(label,edit=()=>{}){const c=structuredClone({label,action:'ready',definition,node,status});edit(c);cases.push(c);}
add('ready');add('repeat-ready',c=>c.action='repeat');add('identity',c=>c.action='identity');
add('management',c=>c.action='management');add('verify-only',c=>c.action='verify');
for(const key of ['principalId','serviceName','connected','synced']){
 add('missing-'+key,c=>delete c.node[key]);
 for(const value of [null,false,'',0,'other'])add(key+'-'+JSON.stringify(value),c=>c.node[key]=value);
}
for(const key of ['serviceNodeId','nativeVersion','nativeExecutableHash','catalogHash','state','epoch','observedAt']){
 for(const value of [null,'other'])add('status-'+key+'-'+JSON.stringify(value),c=>c.status[key]=value);
}
add('other-host-permitted',c=>c.status.hostId='other');
add('native-stopped',c=>c.status.state='stopped');
add('owner-outage',c=>c.failure='serviceNodes.get');
add('discovery-outage',c=>c.failure='tools.list');
add('status-outage',c=>c.failure='tools.call');
add('catalog-mismatch',c=>c.definition.nativePlan.catalogSourceHash='sha256:'+'f'.repeat(64));
add('owner-before-catalog',c=>{c.node.connected=false;c.definition.nativePlan.catalogSourceHash='sha256:'+'f'.repeat(64);});
const rows=[];
for(const c of cases){
 const calls=[];
 const client={async request(method,params){
  calls.push({method,params:structuredClone(params)});
  if(c.failure===method)throw new IvyError('service_unavailable','Synthetic owner outage.','unknown');
  if(method==='serviceNodes.get')return structuredClone(c.node);
  if(method==='tools.list')return {provider:{node:{serviceNodeId:'native-agent'}},items:[{
   qualifiedName:'agent.'+params.namePrefix,definition:{namespace:'agent',name:params.namePrefix,interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}},
   definitionHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null};
  if(method==='tools.call')return structuredClone(c.status);
  throw Error('Unexpected RPC: '+method);
 }};
 const driver=Object.assign(Object.create(ChatNativeDriver.prototype),{store:{client},admission:new ChatAdmission(c.definition),owner});
 let result;
 try{
  let value;
  if(c.action==='identity')value={target:driver.nativeOwner().target,caller:driver.nativeOwner().caller};
  else if(c.action==='verify')value=(await driver.verifyOwner(),null);
  else if(c.action==='management')value=await driver.management('custom',{original:'\ud800'});
  else {value=await driver.ready();if(c.action==='repeat')value=await driver.ready();}
  result={value};
 }catch(cause){const e=IvyError.from(cause);result={error:{code:e.code,message:e.message,outcome:e.outcome}};}
 rows.push({...c,owner,calls,result});
}
process.stdout.write(JSON.stringify(rows));
