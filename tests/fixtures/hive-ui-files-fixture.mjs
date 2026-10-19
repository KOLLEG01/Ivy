// OS fixture setup only. Neither native storage policy nor its results run here.
import {mkdirSync,writeFileSync,utimesSync,rmSync,symlinkSync,linkSync,lstatSync,readdirSync,readFileSync} from 'node:fs';
import {resolve,dirname,relative} from 'node:path';
import {pathToFileURL} from 'node:url';

export function setup(root,actions){
 for(const a of actions){
  const target=resolve(root,a.path);
  switch(a.kind){
   case 'mkdir':mkdirSync(target,{recursive:true});break;
   case 'write':mkdirSync(dirname(target),{recursive:true});writeFileSync(target,Buffer.from(a.data,'base64'));break;
   case 'mtime':utimesSync(target,a.ms/1000,a.ms/1000);break;
   case 'remove':rmSync(target,{recursive:true,force:true});break;
   case 'junction':mkdirSync(dirname(target),{recursive:true});symlinkSync(resolve(root,a.target),target,process.platform==='win32'?'junction':'dir');break;
   case 'hardlink':mkdirSync(dirname(target),{recursive:true});linkSync(resolve(root,a.target),target);break;
   default:throw Error('Unknown OS fixture action');
  }
 }
}
export function tree(root){
 const rows=[];
 const walk=directory=>{
  let names;try{names=readdirSync(directory);}catch(e){if(e.code==='ENOENT')return;throw e;}
  for(const name of names){
   const path=resolve(directory,name),info=lstatSync(path),key=relative(root,path).replaceAll('\\','/');
   if(info.isSymbolicLink()){rows.push({path:key,kind:'link'});continue;}
   if(info.isDirectory()){rows.push({path:key,kind:'directory'});walk(path);}
   else if(info.isFile())rows.push({path:key,kind:'file',data:readFileSync(path).toString('base64')});
   else throw Error('Unexpected fixture file type');
  }
 };
 walk(root);return rows.sort((a,b)=>Buffer.compare(Buffer.from(a.path),Buffer.from(b.path)));
}
if(process.argv[1]&&import.meta.url===pathToFileURL(resolve(process.argv[1])).href){
 const input=JSON.parse(readFileSync(0,'utf8'));setup(input.root,input.actions);
}
