import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {createHash} from 'node:crypto';
import {IvyError} from '../../packages/contracts/src/errors.ts';
const ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript');
const source=readFileSync(new URL('../../packages/host-runtime/src/runtime-reset.ts',import.meta.url),'utf8');
const ast=ts.createSourceFile('runtime-reset.ts',source,ts.ScriptTarget.ESNext,true);
const names=['retainedResetIdentity','collectScopeRoots','configuredScopeRootIds','coordinatedRuntimeResetPlans','releaseResetHosts','bootstrapResetUis'];
const declarations=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.includes(n.name?.text)).map(n=>n.getText(ast).replace(/^export\s+/,''));
if(declarations.length!==names.length)throw Error('Exact original reset functions missing');
const text=declarations.join('\n'),body=ts.transpileModule(text+'\nreturn {retainedResetIdentity,configuredScopeRootIds,coordinatedRuntimeResetPlans,releaseResetHosts,bootstrapResetUis};',{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
const original=new Function('IvyError',body)(IvyError);
const hash='sha256:'+'a'.repeat(64),cases=[];
const add=async(group,name,input,run)=>{const events=[];let outcome;try{outcome={ok:true,value:(await run(events))??null};}catch(e){outcome={ok:false,error:IvyError.from(e).toWire()};}cases.push({group,name,input,expected:{outcome,events}});};
for(const [name,previous]of [
 ['none',null],['held',{resetId:'reset-one',runtimeEpoch:'epoch-one',phase:'complete',planHash:hash}],
 ['released',{resetId:'reset-one',runtimeEpoch:'epoch-one',phase:'released',planHash:hash,deploymentComplete:false}],
 ['confirmed',{resetId:'reset-one',runtimeEpoch:'epoch-one',phase:'released',planHash:hash,deploymentComplete:true}],
 ['different-plan',{resetId:'reset-one',runtimeEpoch:'epoch-one',phase:'prepared',planHash:'different'}],
 ['completion-string',{resetId:'reset-one',runtimeEpoch:'epoch-one',phase:'released',planHash:hash,deploymentComplete:'yes'}],
 ['completion-empty-array',{resetId:'reset-one',runtimeEpoch:'epoch-one',phase:'released',planHash:hash,deploymentComplete:[]}],
 ['completion-zero',{resetId:'reset-one',runtimeEpoch:'epoch-one',phase:'complete',planHash:hash,deploymentComplete:0}],
 ['completion-empty-string',{resetId:'reset-one',runtimeEpoch:'epoch-one',phase:'complete',planHash:hash,deploymentComplete:''}],
])await add('identity',name,{previous,hash},()=>original.retainedResetIdentity(previous,hash));
for(const [name,config]of [
 ['nested',{instances:[{settings:{identity:{rootObjectId:'scope-b'},nested:[{rootObjectId:'scope-a'},{rootObjectId:'scope-b'}],invalid:{rootObjectId:null}}}]}],
 ['utf16',{instances:[{settings:{a:{rootObjectId:'😀'},b:{rootObjectId:'\ud800'},c:{rootObjectId:'a'},d:{rootObjectId:'A'},e:{rootObjectId:''}}}]}],
 ['empty',{instances:[]}],
])await add('scopes',name,{config},()=>original.configuredScopeRootIds(config));
for(const [name,plans]of [['empty',[]],['union',[{hostId:'one',scopeRootIds:['scope-b'],retained:'one'},{hostId:'two',scopeRootIds:['scope-a','scope-b'],retained:'two'}]]])await add('plans',name,{plans},()=>original.coordinatedRuntimeResetPlans(plans));
const hosts=[{hostId:'one'},{hostId:'two'},{hostId:'three'}];
for(const group of ['release','bootstrap'])for(const [name,failAt,afterFailure,reprepareFailures]of [
 ['success',null,false,[]],['lost-first','one',false,[]],['lost-second','two',false,[]],['lost-third','three',false,[]],
 ['after-failure',null,true,[]],['reprepare-partial','two',false,['one']],['reprepare-all',null,true,['one','two','three']],
 ['utf16-error','two',false,[]],
]){
 const message=name==='utf16-error'?'lost \ud800 response':'Synthetic lost phase response.';
 const failure={code:'outcome_unknown',message,outcome:'unknown',...(name==='reprepare-partial'?{details:{operationId:'retained-original-operation'}}:{})};
 const input={hosts,failAt,afterFailure,reprepareFailures,failure};
 await add(group,name,input,async events=>{
  const phase=async host=>{events.push(group+' '+host.hostId);if(host.hostId===failAt)throw new IvyError(failure.code,failure.message,failure.outcome,failure.details);};
  const reprepare=async host=>{events.push('prepare '+host.hostId);if(reprepareFailures.includes(host.hostId))throw new Error('Synthetic failed reprepare');};
  const after=async()=>{events.push('after');if(afterFailure)throw new IvyError(failure.code,failure.message,failure.outcome,failure.details);};
  if(group==='release')await original.releaseResetHosts(hosts,phase,reprepare,after);
  else await original.bootstrapResetUis(hosts,phase,reprepare,after);
 });
}
process.stdout.write(JSON.stringify({sourceHash:'sha256:'+createHash('sha256').update(source).digest('hex'),declarationHash:'sha256:'+createHash('sha256').update(text).digest('hex'),declarations:names,cases}));
