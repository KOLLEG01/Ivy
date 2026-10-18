// Run the original verifier with deterministic filesystem and command adapters.
// No executable or shell is started. The original Windows path logic is retained.
import { registerHooks } from 'node:module';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
const suffix='/packages/host-runtime/src/codex-windows-shell.js';
const hook=registerHooks({
  resolve(specifier,context,next) {
    if(context.parentURL?.endsWith(suffix)) {
      const key={'node:fs':'fs','node:fs/promises':'fsp','node:path':'path','./process.js':'process'}[specifier];
      if(key)return {url:'ivy-shell-fixture:'+key,shortCircuit:true};
    }
    return next(specifier,context);
  },
  load(url,context,next) {
    const source={
      'ivy-shell-fixture:fs':`export async function* createReadStream(){yield Buffer.from('shell fixture');}`,
      'ivy-shell-fixture:fsp':`export async function realpath(path){return path;} export async function stat(){return {isFile:()=>true};}`,
      'ivy-shell-fixture:path':`import {win32} from 'node:path'; export const {basename,dirname,isAbsolute,resolve}=win32;`,
      'ivy-shell-fixture:process':`export async function runCommand(command,cwd,executables,context){globalThis.shellCommands.push({command,cwd,executables,context});return globalThis.shellResult;}`,
    }[url];
    return source===undefined?next(url,context):{format:'module',source,shortCircuit:true};
  },
});
const platform=Object.getOwnPropertyDescriptor(process,'platform');
Object.defineProperty(process,'platform',{...platform,value:'win32'});
const {verifyWindowsShell}=await import('../../dist/packages/host-runtime/src/codex-windows-shell.js');
const {IvyError}=await import('../../dist/packages/contracts/src/errors.js');
const executable='C:\\fixture\\pwsh.exe',hash='sha256:'+createHash('sha256').update('shell fixture').digest('hex');
const observed={packageStatus:15700,version:'7.6.5',executable};
const inputs=[];
const add=(name,value,exitCode=0)=>inputs.push({name,stdout:JSON.stringify(value),exitCode});
for(const version of ['7.6.5','00.01.002','7.6.5-preview.1','7.6.5+build','7.6.5-','7.6.5+','7.6.5-..','7.6.5--','7.6.5+-','7.6.5-a+b','7.6.5+a-b','7.6','7.6.5.1','7.6.5\n','7.6.5\r','7.6.5\u2028','7.6.5\u2029',' 7.6.5','7.6.5 ','\ufeff7.6.5','٧.٦.٥','7.6.5-ä','7.6.5_1','','v7.6.5','7.6.5\0','7.6.5-\ud800','7.6.5+AZaz09.-',null,7,[],{},true])add('version:'+JSON.stringify(version),{...observed,version});
for(const packageStatus of [0,122,'15700',null,{},[],true,15700.1])add('package:'+JSON.stringify(packageStatus),{...observed,packageStatus});
for(const value of [null,[],true,false,0,'text',{}, {version:'bad'}, {packageStatus:15700}, {...observed,executable:null}, {...observed,executable:4}])add('shape:'+JSON.stringify(value),value);
for(const exitCode of [1,15,null])add('exit:'+exitCode,observed,exitCode);
for(const stdout of ['', 'not json', '\ufeff'+JSON.stringify(observed), '{', '[]\n', JSON.stringify(observed)+'\n', JSON.stringify(observed).replace('15700','1.57e4'), JSON.stringify(observed).replace('15700','122,"packageStatus":15700')])inputs.push({name:'json:'+stdout,stdout,exitCode:0});
globalThis.shellCommands=[];
const rows=[];
for(const input of inputs){
  globalThis.shellResult={...input,stderr:'',signal:null,truncated:false};
  let expected;
  try{const result=await verifyWindowsShell({executable,executableHash:hash},'C:\\fixture','C:\\fixture\\ivy-job.exe');expected={result:{version:result.version,executable:result.executable}};}
  catch(error){expected={error:IvyError.from(error).toWire()};}
  rows.push({...input,expected});
}
assert.equal(globalThis.shellCommands.length,rows.length);
const command=globalThis.shellCommands[0];
assert.ok(globalThis.shellCommands.every(c=>JSON.stringify(c)===JSON.stringify(command)));
assert.ok(rows.filter(r=>r.name.startsWith('exit:')).every(r=>r.expected.result));
hook.deregister();Object.defineProperty(process,'platform',platform);
process.stdout.write(JSON.stringify({rows,command}));
