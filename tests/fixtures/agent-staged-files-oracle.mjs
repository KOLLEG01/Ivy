import {mkdir,readdir,lstat,readFile,writeFile,utimes,symlink,readlink} from 'node:fs/promises';
import {rmSync,realpathSync} from 'node:fs';
import {join,relative,dirname,basename} from 'node:path';
import {createHash} from 'node:crypto';
import {stageFile} from '../../dist/services/agent-manager/src/staged-files.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {validateAgent} from '../../dist/packages/contracts/src/agent-validation.js';
import {inside} from '../../dist/packages/host-runtime/src/config.js';

let raw='';for await(const part of process.stdin)raw+=part;const request=JSON.parse(raw);
const now=Date.parse('2026-10-18T00:00:00.000Z'),retention=30*24*60*60*1000;
const cases=[
  {name:'canonical-byte',input:{name:'hello.txt',dataBase64:'eA=='}},
  {name:'empty-file',input:{name:'empty',dataBase64:''}},
  {name:'three-bytes',input:{name:'one/two.txt',dataBase64:'YWJj'}},
  {name:'binary-bytes',input:{name:'binary',dataBase64:'AP+A'}},
  ...['eA','eA=','eA===','eB==','e A==','eA==\n','eA-_','!!!!','\ud800'].map((dataBase64,i)=>({name:'noncanonical-'+i,input:{name:'bad',dataBase64}})),
  {name:'sanitized-reserved',input:{name:'../CON.',dataBase64:'eA=='}},
  {name:'empty-sanitized-name',input:{name:' \t...',dataBase64:'eA=='}},
  {name:'forbidden-characters',input:{name:'one/x<bad?.txt',dataBase64:'eA=='}},
  {name:'utf16-slice',input:{name:'a'.repeat(127)+'😀',dataBase64:'eA=='}},
  {name:'approved-cap-and-old-path',input:{name:'con.'+'x'.repeat(124),dataBase64:'eA=='},legacy:true,approvedCap:true},
  {name:'expiry-boundary-readonly-and-links',input:{name:'expiry',dataBase64:'eA=='},expiry:true},
  {name:'maximum-bytes',input:{name:'maximum'},fill:120,byteLength:8*1024*1024},
  {name:'over-maximum-bytes',input:{name:'too-large'},fill:120,byteLength:8*1024*1024+1},
  {name:'invalid-schema-before-codec',input:{name:'invalid',dataBase64:'eA==',extra:true}},
];
const digest=bytes=>'sha256:'+createHash('sha256').update(bytes).digest('hex');
function input(row){return row.byteLength===undefined?row.input:{...row.input,dataBase64:Buffer.alloc(row.byteLength,row.fill).toString('base64')};}
function normalize(value,root){if(typeof value==='string')return value.replaceAll(root,'<root>').replaceAll('\\','/');if(Array.isArray(value))return value.map(v=>normalize(v,root));if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v,root)]));return value;}
async function prepare(root,row){
  const staged=join(root,'staged');await mkdir(staged,{recursive:true});let legacy=null;
  if(row.legacy)legacy=await stageFile(staged,input(row),now);
  if(row.expiry){
    for(const [name,delta] of [['expired',-retention-1],['boundary',-retention],['future',1000]]){
      const directory=join(staged,name);await mkdir(directory);await writeFile(join(directory,'keep'),'preserve');await utimes(directory,new Date(now+delta),new Date(now+delta));
      if(name==='expired'){const {chmod}=await import('node:fs/promises');await chmod(join(directory,'keep'),0o444);await chmod(directory,0o555);}
    }
    await writeFile(join(staged,'ordinary-file'),'retain');
    const outside=join(root,'outside');await mkdir(outside);await writeFile(join(outside,'keep'),'outside bytes');
    await symlink(outside,join(staged,'outside-link'),process.platform==='win32'?'junction':'dir');
  }
  const data=input(row),decoded=typeof data.dataBase64==='string'?Buffer.from(data.dataBase64,'base64'):null;
  return {input:data,now,staged,legacy,codec:decoded&&decoded.toString('base64')===data.dataBase64?{canonical:true,byteLength:decoded.length,...(row.fill===undefined?{bytes:[...decoded]}:{fill:row.fill})}:{canonical:false}};
}
async function snapshot(root){
  const rows=[];
  async function visit(path){for(const entry of await readdir(path,{withFileTypes:true})){
    const child=join(path,entry.name),name=relative(root,child).replaceAll('\\','/');
    if(entry.isSymbolicLink()){rows.push({path:name,kind:'link',target:await readlink(child)});continue;}
    if(entry.isDirectory()){const info=await lstat(child);rows.push({path:name,kind:'directory',...(name.startsWith('staged/')&&!name.slice(7).includes('/')?{mtimeMs:info.mtimeMs}:{})});await visit(child);}
    else {const bytes=await readFile(child);rows.push({path:name,kind:'file',byteLength:bytes.length,contentHash:digest(bytes),...(bytes.length<16?{bytes:[...bytes]}:{})});}
  }}
  await visit(root);rows.sort((a,b)=>a.path<b.path?-1:a.path>b.path?1:0);return normalize(rows,root);
}
if(request.action==='catalog')console.log(JSON.stringify(cases));
else if(request.action==='prepare')console.log(JSON.stringify(await prepare(request.root,request.case)));
else if(request.action==='snapshot')console.log(JSON.stringify(await snapshot(request.root)));
else if(request.action==='normalize')console.log(JSON.stringify(normalize(request.value,request.root)));
else if(request.action==='cleanup'){
  const root=realpathSync(request.root),parent=realpathSync(request.parent);
  if(!inside(parent,root)||root===parent||!/^stage-(original|rust)-/.test(basename(root)))throw Error('Unsafe staging fixture cleanup');
  rmSync(root,{recursive:true,force:true});console.log('null');
}else{
  const {root,case:row}=request,prepared=await prepare(root,row);let outcome,originalFailure=null;
  try{
    validateAgent('StageFileInput',prepared.input);
    let result=await stageFile(prepared.staged,prepared.input,prepared.now);
    if(row.approvedCap){
      try{validateAgent('StagedFile',result);}catch(error){originalFailure={result,outputError:IvyError.from(error).toWire()};}
      if(result.name.length!==129||!originalFailure)throw Error('Original reserved overflow proof changed');
      const name=result.name.slice(0,128),path=join(dirname(result.path),name);
      // The one explicit parent-approved behavior exception. Retain the old
      // file and add the expected new path; do not pretend the original did so.
      await writeFile(path,await readFile(result.path));result={...result,name,path};
      await utimes(dirname(path),new Date(now),new Date(now));
    }
    validateAgent('StagedFile',result);outcome={result};
  }catch(error){outcome={error:IvyError.from(error).toWire()};}
  console.log(JSON.stringify(normalize({outcome,snapshot:await snapshot(root),legacy:prepared.legacy,originalFailure},root)));
}
