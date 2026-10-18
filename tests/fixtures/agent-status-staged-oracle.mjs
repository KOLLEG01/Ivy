import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import {NativeJournal} from '../../dist/services/agent-manager/src/journal.js';
import {stagedName} from '../../dist/services/agent-manager/src/staged-files.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest} from '../../dist/packages/contracts/src/canonical.js';
const source=readFileSync('services/agent-manager/src/main.ts','utf8');
const file=ts.createSourceFile('main.ts',source,ts.ScriptTarget.Latest,true);
let expression;
function visit(node){if(ts.isVariableDeclaration(node)&&node.name.getText(file)==='status'&&node.initializer&&ts.isArrowFunction(node.initializer))expression=node.initializer.getText(file);ts.forEachChild(node,visit);}
visit(file);if(!expression)throw Error('Actual status initializer missing');
const time='2026-10-18T00:00:00.000Z',RealDate=Date;
class FixedDate extends RealDate{constructor(...args){super(...(args.length?args:[time]));}static now(){return RealDate.parse(time);}}
globalThis.Date=FixedDate;
const owner={serviceNodeId:'agent',nativeVersion:'0.154.0',nativeExecutableHash:digest('status fixture')};
const limits={maxOperations:100,maxJournalBytes:128*1024*1024,maxPendingInputs:16};
function journal(seed){
  const j=new NativeJournal(owner,limits);
  if(seed!=='empty')j.beginEpoch('epoch');
  if(['operation','input','lost','closed'].includes(seed))j.accept({callerPrincipalId:'caller',operationId:'operation'},'turn/start',{threadId:'thread'});
  if(['input','lost','closed'].includes(seed))j.observeInput({serviceNodeId:'agent',epoch:'epoch',requestId:7},'currentTime/read',{threadId:'thread'});
  if(seed==='lost')j.loseEpoch('epoch','native_unavailable');
  if(seed==='closed')j.close();
  return j;
}
const base={config:{serviceNodeId:'agent',hostId:'host'},settings:{nativeVersion:owner.nativeVersion,nativeExecutableHash:owner.nativeExecutableHash},catalogHash:digest('catalog fixture'),processStops:[{epoch:'prior',observedAt:'2026-10-17T00:00:00.000Z'}],owner:{launcherPid:42,initialized:true},closing:false,connected:true,ready:true,threadCode:null,projectCode:null,capabilities:{serviceNodeId:'agent',hostId:'host',revision:2,capabilities:[{key:'test',label:'Retained'}],operationId:'operation',updatedAt:time}};
const cases=[];
function add(name,patch={},seed='live',windows=false){
  const input={...structuredClone(base),...structuredClone(patch),seed,windows};
  const native=structuredClone(input.owner);
  if(native!==null)native.rpc={connected:input.connected};
  const manager=key=>input[key+'Present']||input[key]?{status:input[key]}:null;
  const context={config:input.config,settings:input.settings,catalogHash:input.catalogHash,owner:native,journal:journal(seed),closing:input.closing,service:{ready:input.ready},processStops:input.processStops,process:{platform:windows?'win32':'linux'},Date:FixedDate,threadInventoryCode:input.threadCode,projectInventoryCode:input.projectCode,capabilityState:{value:input.capabilities},instructions:manager('instructions'),mcp:manager('mcp'),skills:manager('skills')};
  let expected;try{expected={result:vm.runInNewContext(ts.transpile(`const originalStatus=${expression}; originalStatus();`,{target:ts.ScriptTarget.ES2022}),context)};}catch(error){expected={error:IvyError.from(error).toWire()};}
  cases.push({name,input,expected:JSON.parse(JSON.stringify(expected))});
}
for(const seed of ['empty','live','operation','input','lost','closed'])for(const windows of [false,true])for(const closing of [false,true])for(const connected of [false,true])for(const ready of [false,true])add(`${seed}-${windows}-${closing}-${connected}-${ready}`,{closing,connected,ready},seed,windows);
for(const mode of [undefined,null,'owned-stdio','external-proxy','claude-adapter',''])for(const daemon of [undefined,null,{}, {pid:123,status:'alreadyRunning'},{pid:null,status:null},{pid:0,status:''}]){
  const native={...base.owner,nativeHome:'C:\\native',ownsNativeHome:true,socketPath:'C:\\socket'};
  if(mode!==undefined)native.connectionMode=mode;if(daemon!==undefined)native.daemon=daemon;
  add(`connection-${String(mode)}-${JSON.stringify(daemon)}`,{owner:native});
}
for(const field of ['nativeHome','ownsNativeHome','actualServerVersion','socketPath','windowsShell','initialized','launcherPid'])for(const value of [undefined,null,false,0,'',true,'observed',{},[]]){
  const native={...base.owner,nativeHome:'C:\\native'};
  if(value===undefined)delete native[field];else native[field]=value;
  add(`owner-${field}-${JSON.stringify(value)}`,{owner:native});
}
for(const [threadCode,projectCode] of [[null,null],['thread','project'],[null,'project'],['', 'project'],['thread',null]])add(`codes-${threadCode}-${projectCode}`,{threadCode,projectCode});
for(const setting of [{},{appServer:{mode:'claude-adapter'}},{appServer:null},{appServer:{mode:'external-proxy'}}])add(`settings-${JSON.stringify(setting)}`,{settings:{...base.settings,...setting}});
for(const instructions of [null,{phase:'ready'},{}])for(const mcp of [null,{phase:'disabled'}])for(const skills of [null,{phase:'ready'}])add(`environment-${JSON.stringify([instructions,mcp,skills])}`,{instructions,mcp,skills});
for(const value of [null,false,0,''])add(`present-manager-status-${JSON.stringify(value)}`,{instructionsPresent:true,mcpPresent:true,skillsPresent:true,instructions:value,mcp:value,skills:value});
add('retained-null-capabilities',{capabilities:null});add('owner-null',{owner:null});
const names=['','..','. . .','../../etc/passwd','C:\\temp\\..','con','CON.txt','prn','AUX','nul','com0','com9.txt','com10.txt','LPT0.x','Lpt9','LPT10','conspiracy','a<b>:c?.png','file/','file\\',' \uFEFFhello\u3000.','\u0085hello\u0085','\ud800name\udfff','x'.repeat(129),'😀'.repeat(65),'a'.repeat(127)+'😀','con.'+'x'.repeat(124),'../CON.'+'x'.repeat(124)];
for(let unit=0;unit<32;unit++)names.push(String.fromCharCode(unit)+'x'+String.fromCharCode(unit));
for(const unit of [9,10,11,12,13,32,160,5760,8192,8193,8194,8195,8196,8197,8198,8199,8200,8201,8202,8232,8233,8239,8287,12288,65279,0x85,0x180e,0x200b,0xd800,0xdc00])names.push('.'+String.fromCharCode(unit)+'name'+String.fromCharCode(unit)+'.');
if(cases.filter(row=>row.expected.error).length!==1)throw Error('Original status corpus did not execute the actual successful expression');
console.log(JSON.stringify({owner,limits,time,statusExpression:expression,cases,names:names.map(name=>({name,result:stagedName(name)}))}));
