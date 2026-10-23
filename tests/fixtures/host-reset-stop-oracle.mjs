import assert from 'node:assert/strict';
import {readFileSync,existsSync} from 'node:fs';
import {mkdir,readFile,lstat} from 'node:fs/promises';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {registerHooks,createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {createHash} from 'node:crypto';
const input=JSON.parse(readFileSync(0,'utf8')),root=path.resolve(input.fixtureRoot);
assert(path.basename(root).startsWith('ivy-config-'));
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {atomicJson,jsonFile}=await import('../../packages/host-runtime/src/config.ts');
const {HostJournal,ExecutorLock}=await import('../../packages/host-runtime/src/journal.ts');
const {runtimeResetMarker}=await import('../../packages/host-runtime/src/runtime-maintenance.ts');
const {servicePaths}=await import('../../packages/host-runtime/src/layout.ts');
const {stopContainer}=await import('../../packages/host-runtime/src/docker.ts');
const {defaultSystemdAdapter,systemdUnitName}=await import('../../packages/host-runtime/src/systemd.ts');
const source=readFileSync(new URL('../../packages/host-runtime/src/runtime-reset.ts',import.meta.url),'utf8');
const ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript'),ast=ts.createSourceFile('reset.ts',source,ts.ScriptTarget.ESNext,true);
const names=['requireAdministrator','sendStop','alive','waitStopped','stopHost','savedRuntime','prepareLocal','completeLocal','confirmLocal','proveLocksReleased','remote'];
const declarations=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.includes(n.name?.text)).map(n=>n.getText(ast));assert.equal(declarations.length,names.length);
const variables=ast.statements.filter(n=>ts.isVariableStatement(n)&&n.declarationList.declarations.some(d=>['resetDirectory','recordPath'].includes(d.name.getText(ast)))).map(n=>n.getText(ast));assert.equal(variables.length,2);
const NativeDate=Date;let ticks=0;
class FixtureDate extends NativeDate{constructor(...args){super(...(args.length?args:[input.deadlineClock?(ticks++)*100_000:input.at]));}static now(){return input.deadlineClock?(ticks++)*100_000:NativeDate.parse(input.at);}}
const scope={requireThat,IvyError,execute:promisify(execFile),jsonFile,atomicJson,hashJson,validateHost,HostJournal,ExecutorLock,runtimeResetMarker,servicePaths,stopContainer,defaultSystemdAdapter,systemdUnitName,mkdir,join:path.join,dirname:path.dirname,Date:FixtureDate,process,setTimeout};
const body=ts.transpileModule(variables.join('\n')+'\n'+declarations.join('\n')+'\nreturn {requireAdministrator,sendStop,alive,waitStopped,stopHost,prepareLocal,completeLocal,confirmLocal,remote};',{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
const original=new Function(...Object.keys(scope),body)(...Object.values(scope));
const sha=b=>'sha256:'+createHash('sha256').update(b).digest('hex');
async function contents(file){try{const m=await lstat(file);if(m.isDirectory())return{directory:true};return{base64:(await readFile(file)).toString('base64')};}catch(error){if(error.code==='ENOENT')return null;throw error;}}
async function observe(){
 const data=servicePaths(input.config,input.instance).data;
 const health=await contents(path.join(data,'health.json')),control=await contents(path.join(data,'control.json'));
 const marker=await contents(runtimeResetMarker(input.config));
 const record=await contents(path.join(path.dirname(runtimeResetMarker(input.config)),'current.json'));
 return{healthUnchanged:JSON.stringify(health)===JSON.stringify(input.healthBefore),control,markerUnchanged:JSON.stringify(marker)===JSON.stringify(input.markerBefore),recordUnchanged:JSON.stringify(record)===JSON.stringify(input.recordBefore)};
}
if(input.kind==='observe')process.stdout.write(JSON.stringify(await observe()));
else{
 let value;
 try{
  if(input.operation==='admin')value=await original.requireAdministrator();
  else if(input.operation==='send')value=await original.sendStop(input.config,input.instance);
  else if(input.operation==='alive')value=original.alive(input.pid);
  else if(input.operation==='wait')value=await original.waitStopped(input.pids);
  else if(input.operation==='stop')value=await original.stopHost(input.config);
  else if(input.operation==='prepare')value=await original.prepareLocal(input.config,input.plan,input.resetId,input.runtimeEpoch,input.publisherDigest);
  else if(input.operation==='complete')value=await original.completeLocal(input.config,input.resetId,input.runtimeEpoch);
  else if(input.operation==='confirm')value=await original.confirmLocal(input.config,input.resetId,input.runtimeEpoch);
  else if(input.operation==='remote')value=await original.remote(input.config,input.host,input.args);
  else assert.fail('Unknown reset host operation');
  value={ok:true,value:value===undefined&&input.operation==='remote'?'fixture-undefined-data':value??null};
 }catch(error){value={ok:false,error:IvyError.from(error).toWire()};}
 process.stdout.write(JSON.stringify({outcome:value,state:await observe(),sourceHash:sha(source),declarationHash:sha(variables.join('\n')+declarations.join('\n')),excludedStatements:[]}));
}
