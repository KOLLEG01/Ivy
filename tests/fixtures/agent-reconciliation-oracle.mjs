import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import ts from 'typescript';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
import {LocalAgentState} from '../../dist/services/agent-manager/src/hive-state.js';
const source=readFileSync('services/agent-manager/src/main.ts','utf8'),file=ts.createSourceFile('main.ts',source,ts.ScriptTarget.Latest,true),found={};
function visit(node){
  if(ts.isPropertyAssignment(node)&&['reconcile','readiness'].includes(node.name.getText(file)))found[node.name.getText(file)]=node.initializer.getText(file);
  if(ts.isVariableDeclaration(node)&&['available','probe','writeHealth'].includes(node.name.getText(file)))found[node.name.getText(file)]=node.initializer.getText(file);
  if(ts.isCallExpression(node)&&node.expression.getText(file)==='requireThat'&&node.arguments[0]?.getText(file)==='probe')found.requireProbe=node.getText(file);
  ts.forEachChild(node,visit);
}visit(file);for(const name of ['reconcile','readiness','available','probe','writeHealth','requireProbe'])assert.ok(found[name],name);
const js=text=>ts.transpile(text,{target:ts.ScriptTarget.ES2022});
const create=Function('env','IvyError','requireThat','hashJson','with(env){'+js('const available='+found.available+';const probe='+found.probe+';'+found.requireProbe+';return {reconcile:'+found.reconcile+',readiness:'+found.readiness+'};')+'}');
const catalog=JSON.parse(readFileSync('specs/native/codex-0.154.0/catalog.json','utf8'));
// The fixture intentionally uses a complete original native definition inside
// a registry. Its hash must cover every field, rather than a selected projection.
const definition={name:'account/rateLimits/read',nativeMethod:'account/rateLimits/read',description:'Null-argument probe\ud800',inputSchema:{type:'null'},extra:{exact:'\udfff'}};
const registry={namespaces:[{tools:[definition]}]},time='2026-10-18T00:00:00.000Z',RealDate=Date;
let milliseconds=42000;globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[time]));}static now(){return milliseconds;}};
const failure=(code='fixture_failure',outcome='not_executed')=>({code,message:'Original failure\ud800',outcome,details:{retained:'\udfff'}});
const error=f=>new IvyError(f.code,f.message,f.outcome,f.details);
const capture=async task=>{try{return{result:(await task())??null};}catch(e){return{error:IvyError.from(e).toWire()};}};
const cases=[];
function add(name,changes={}){cases.push({name,probe:failure('service_not_ready'),epoch:'epoch',ownerEpoch:'epoch',runtimeEpoch:'hive-epoch',connected:true,closing:false,environment:false,...changes});}
add('compatible-fence');add('compatible-fence-with-environment',{environment:true});
for(const outcome of ['unknown','completed'])add('wrong-fence-outcome-'+outcome,{probe:failure('service_not_ready',outcome)});
for(const code of ['invalid_arguments','outcome_unknown','service_not_ready\ud800'])add('wrong-fence-code-'+code,{probe:failure(code)});
for(const probe of [null,{},[null,1]])add('unexpected-probe-success-'+JSON.stringify(probe),{probe});
add('missing-pinned-probe',{missingProbe:true});add('closing-owner',{closing:true});add('disconnected-owner',{connected:false});add('stale-owner-epoch',{ownerEpoch:'other'});
add('invalid-hive-epoch-empty',{runtimeEpoch:''});add('invalid-hive-epoch-colon',{runtimeEpoch:'invalid:epoch'});add('status-request-failure',{statusFailure:failure('status_unavailable','unknown')});
const rows=[];
for(const row of cases){
 const calls=[],locations=[],environment=[],refreshes=[];
 const connection={generation:7,request:async(method,params)=>{calls.push({method,params});if(method==='system.status'){if(row.statusFailure)throw error(row.statusFailure);return{runtimeEpoch:row.runtimeEpoch};}if(row.probe?.code)throw error(row.probe);return row.probe;}};
 const state={runtimeEpoch:null,setRuntimeEpoch:LocalAgentState.prototype.setRuntimeEpoch};
 const env={config:{serviceNodeId:'owner'},registry:row.missingProbe?{namespaces:[{tools:[]}]}:registry,
  owner:{rpc:{connected:row.connected},epoch:row.ownerEpoch},journal:{epoch:row.epoch},closing:row.closing,state,hiveConnection:null,
  inventory:{currentProjects:()=>({projects:{projects:[]}})},locations:{setNativeProjects:projects=>locations.push(projects)},
  environmentAt:0,...Object.fromEntries(['instructions','mcp','skills'].map(name=>[name,row.environment?{synchronize:()=>{environment.push(name);return Promise.resolve();}}:null])),
  refreshThreads:async()=>{refreshes.push('threads');},refreshProjects:async()=>{refreshes.push('projects');}};
 const outcome=await capture(async()=>{await create(env,IvyError,requireThat,hashJson).reconcile(connection);});
 rows.push({...row,expected:{outcome,calls,locations,environment,refreshes,bound:env.hiveConnection!==null,runtimeEpoch:state.runtimeEpoch,environmentAt:env.environmentAt}});
}
const readiness=[];
for(const closing of [false,true])for(const connected of [false,true])for(const [threadCode,projectCode] of [[null,null],['thread-failed','project-failed'],[null,'project-failed'],['', 'project-failed'],['\ud800',null]]){
 const health=[],env={config:{serviceNodeId:'owner'},registry,owner:{rpc:{connected},epoch:'owner-epoch'},journal:{epoch:'stale-epoch'},closing,
  instructions:null,mcp:null,skills:null,environmentAt:0,threadRefreshWork:null,projectRefreshWork:null,threadRefreshAt:milliseconds,projectRefreshAt:milliseconds,
  threadInventoryCode:threadCode,projectInventoryCode:projectCode,refreshThreads:async()=>{},refreshProjects:async()=>{},writeHealth:async(...args)=>{health.push(args);}};
 const result=await create(env,IvyError,requireThat,hashJson).readiness({generation:7});
 readiness.push({closing,connected,threadCode,projectCode,expected:{result,health}});
}
const warnings=[];
for(const [retained,maximum,bytes,maximumBytes] of [[0,10,0,100],[7,10,79,100],[8,10,0,100],[0,10,80,100],[0,0,0,100],[0,10,0,0]]){
 const calls=[],env={journal:{status:()=>({retained,maximum,bytes,maximumBytes})},healthWork:Promise.resolve(),health:{write:async(...args)=>{calls.push(args);}}};
 const write=Function('env','with(env){'+js('return '+found.writeHealth+';')+'}')(env);await write(true,7,'Original health\ud800');warnings.push({retained,maximum,bytes,maximumBytes,expected:calls[0]});
}
console.log(JSON.stringify({catalog,registry,time,now:milliseconds,rows,readiness,warnings}));
globalThis.Date=RealDate;
