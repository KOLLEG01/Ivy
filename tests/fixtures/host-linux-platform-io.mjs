import {promisify} from 'node:util';
const state=globalThis.__ivyLinuxFixture;
if(!state)throw Error('Linux fixture I/O is not initialized');
const failure=code=>Object.assign(Error('Explicit simulated boundary failure'),{code});
function event(value){
 state.trace.push(value);
 const index=state.counts[value.op]=(state.counts[value.op]??0)+1;
 const fault=state.model.faults?.find(row=>row.op===value.op&&(row.at??1)===index&&(!row.path||row.path===value.path));
 if(fault)throw failure(fault.code??'EIO');
}
export async function lstat(path){
 event({op:'stat',path});
 const file=state.files[path];if(!file)throw failure('ENOENT');if(file.error)throw failure(file.error);
 return {isFile:()=>file.file??true,isSymbolicLink:()=>file.symlink??false,uid:file.uid??0,gid:file.gid??0,mode:file.mode??420,size:file.size??Buffer.byteLength(file.data??'')};
}
export async function readFile(path,encoding){event({op:'read',path});const file=state.files[path];if(!file)throw failure('ENOENT');if(file.readError)throw failure(file.readError);const bytes=Buffer.from(file.data??'');return encoding?bytes.toString(encoding):bytes;}
export async function open(path,flags,mode){
 event({op:'open',path,flags,mode:mode??null});
 if(flags==='wx'){
  if(state.model.createRace){state.files[path]=structuredClone(state.model.createRace);throw failure('EEXIST');}
  if(state.files[path])throw failure('EEXIST');
  state.files[path]={data:'',file:true,symlink:false,uid:0,gid:0,mode,size:0};
 }
 return {
  async writeFile(value){const data=Buffer.from(value).toString('utf8');event({op:'write',path,data});Object.assign(state.files[path],{data,size:Buffer.byteLength(data)});},
  async sync(){event({op:'sync',path});},
  async close(){event({op:'close',path});},
 };
}
export function execFile(){throw Error('Only the explicitly promisified simulated command is available');}
execFile[promisify.custom]=async (file,args,options)=>{
 event({op:'command',file,args,timeoutMs:options.timeout,maxBuffer:options.maxBuffer});
 if(!['/usr/bin/id','/usr/bin/setfacl','/usr/bin/chown','/usr/sbin/useradd','/usr/bin/systemctl'].includes(file))throw Error('Unlisted simulated command');
 const result=state.model.commands?.[state.commandIndex++]??{};
 if(result.error)throw failure(result.error);
 return {stdout:result.stdout??'',stderr:''};
};
