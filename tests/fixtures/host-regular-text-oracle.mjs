import * as fs from 'node:fs/promises';
import {constants,readFileSync} from 'node:fs';
import {relative,isAbsolute as isAbsolute2,sep,resolve,dirname} from 'node:path';
import assert from 'node:assert/strict';
const input=JSON.parse(readFileSync(0,'utf8'));
const source=readFileSync(process.env.IVY_TEST_REGULAR_TEXT_REFERENCE,'utf8');
const select=(start,end)=>{const i=source.indexOf(start),j=source.indexOf(end,i);assert(i>=0&&j>i);return source.slice(i,j);};
const body=select('function isPathInside(', '\nasync function readJournalEntries(')+select('async function readRegularTextFile(', '\nasync function workflowAgentResult(')+select('function isMissingFile(', '\nfunction delay(');
async function effect(stage){
 if(input.stage!==stage)return;
 const root=await fs.realpath(input.guard),path=resolve(input.path),local=relative(root,path);
 assert(local&&!local.startsWith('..'+sep)&&local!=='..'&&!isAbsolute2(local));
 assert.equal(await fs.realpath(dirname(path)),dirname(path));
 if(input.action==='remove')await fs.unlink(path);
 else if(input.action==='replace'){await fs.rename(path,path+'.held');await fs.writeFile(path,'replacement');}
 else if(input.action==='same-inode'){await fs.rename(path,path+'.held');await fs.link(path+'.held',path);}
 else if(input.action==='grow')await fs.appendFile(path,'-late');
 else if(input.action==='symlink'){await fs.rename(path,path+'.held');await fs.symlink(input.target,path,'file');}
 else throw Error('Unknown fixture mutation');
}
let first=true;
const realpath=async path=>{const result=await fs.realpath(path);if(first){first=false;await effect('before-open');}return result;};
const open=async(path,flags)=>{
 const handle=await fs.open(path,flags);
 return {stat:async()=>{const value=await handle.stat();await effect('after-stat');return value;},readFile:async encoding=>{await effect('before-read');return handle.readFile(encoding);},close:()=>handle.close()};
};
try{
 const reader=new Function('realpath','open','lstat','constants','relative','isAbsolute2','sep',body+'\nreturn readRegularTextFile;')(realpath,open,fs.lstat,constants,relative,isAbsolute2,sep);
 const value=input.operation==='realpath'?await fs.realpath(input.path):await reader(input.path,input.directory,input.limit);
 process.stdout.write(JSON.stringify({ok:true,value}));
}catch(error){
 if(input.operation==='realpath'&&['ENOENT','ENOTDIR'].includes(error.code))process.stdout.write(JSON.stringify({ok:true,value:null}));
 else process.stdout.write(JSON.stringify({ok:false,name:error.name,code:error.code??null,message:error.message}));
}
