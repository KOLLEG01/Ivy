import {stat,access,utimes,realpath,readdir} from 'node:fs/promises';
import {readFileSync} from 'node:fs';
const input=JSON.parse(readFileSync(0,'utf8'));
try{
 let value;
 if(input.operation==='identity'){
  const s=await stat(input.path,{bigint:true});value={dev:s.dev.toString(),ino:s.ino.toString()};
 }else if(input.operation==='access')value=await access(input.path,input.mode)??null;
 else if(input.operation==='times'||input.operation==='readTimes'){
  if(input.operation==='times')await utimes(input.path,new Date(input.accessed),new Date(input.modified));
  const s=await stat(input.path);value={accessed:s.atimeMs,modified:s.mtimeMs};
 }else if(input.operation==='directory'){
  await readdir(await realpath(input.path));value=null;
 }else throw Error('Unknown filesystem operation');
 process.stdout.write(JSON.stringify({ok:true,value}));
}catch(e){process.stdout.write(JSON.stringify({ok:false,code:e.code}));}
