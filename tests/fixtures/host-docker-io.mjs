import {mkdir as nativeMkdir} from 'node:fs/promises';
import {readFileSync} from 'node:fs';
import {relative,isAbsolute} from 'node:path';
const state=globalThis.__ivyDockerFixture;
if(!state)throw Error('Docker fixture boundary is not initialized');
function event(value){state.trace.push(value);const at=state.counts[value.op]=(state.counts[value.op]??0)+1;if(state.model.faults?.some(v=>v.op===value.op&&(v.at??1)===at))throw Error('Injected boundary failure.');}
export function next(){const row=state.model.commands?.[state.commandIndex++]??{};if(row.error)throw new state.IvyError(row.error,'Injected command failure.');return {exitCode:row.exitCode??0,signal:null,errorCode:null,stdout:row.stdout??'',stderr:row.stderr??'',truncated:row.truncated??false};}
export async function mkdir(path,options){event({op:'mkdir',path});if(state.model.nativeFiles){contained(path);await nativeMkdir(path,options);}state.directories.push(path);}
function contained(path){const rel=relative(state.root,path);if(!rel||isAbsolute(rel)||rel==='..'||rel.startsWith('../')||rel.startsWith('..\\'))throw Error('Native fixture path leaves its disposable root');}
export async function jsonFile(path){event({op:'configuration',path});if(state.model.configError)throw Error('Injected configuration failure.');if(state.model.nativeFiles){contained(path);return JSON.parse(readFileSync(path,'utf8'));}return state.model.configuration??{};}
export async function verifyLinuxResourceMembership(host,instance,scope){event({op:'resources',instance,scope});}
export {installationIdentity,validateLinuxResourceScope} from '../../packages/host-runtime/src/linux-resources.ts';
export async function runCommand(command,root,executables,options){event({op:'execute',args:command.args,timeoutMs:command.timeoutMs,redact:options.redact});if(state.model.nativeCommands){contained(executables.docker);const {runCommand:native}=await import('../../packages/host-runtime/src/process.ts');return native(command,root,executables,options);}return next();}
export async function setTimeout(milliseconds,_value,options){event({op:'sleep',milliseconds});state.sleepEntered?.();if(state.model.pauseObserver)return new Promise((resolve,reject)=>{if(options.signal.aborted)reject(Error('aborted'));else options.signal.addEventListener('abort',()=>reject(Error('aborted')),{once:true});});if(options.signal.aborted)throw Error('aborted');}
