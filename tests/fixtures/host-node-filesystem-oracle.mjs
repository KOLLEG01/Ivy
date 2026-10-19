import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
const input=JSON.parse(fs.readFileSync(0,'utf8'));
if(input.operation==='paths'){
 const cwd=process.cwd,env=process.env;
 try{
  process.stdout.write(JSON.stringify(input.cases.map(c=>{
   process.cwd=()=>c.cwd;process.env=new Proxy({}, {get:(_,key)=>typeof key==='string'&&key.startsWith('=')?c.drives[key.slice(1).toUpperCase()]:env[key]});
   return Buffer.from(path[c.style].toNamespacedPath(c.path),'utf8').toString('utf8');
  })));
 }finally{process.cwd=cwd;process.env=env;}
}else{
 const root=path.join(input.root,'original');fs.mkdirSync(root,{recursive:true});
 for(const c of input.cases){
  let logical=path.join(root,c.path);
  if(c.forward)logical=logical.replaceAll('\\','/');
  if(c.verbatim)logical=path.toNamespacedPath(logical);
  fs.mkdirSync(path.dirname(logical),{recursive:true});fs.writeFileSync(logical,Buffer.from([1,2,3]),{flag:'wx'});
  if(!fs.readFileSync(logical).equals(Buffer.from([1,2,3])))throw Error('Original bytes changed');
 }
 const values=[];function scan(directory,prefix=''){for(const entry of fs.readdirSync(directory,{withFileTypes:true})){const name=prefix+entry.name,file=path.join(directory,entry.name);if(entry.isDirectory())scan(file,name+'/');else{const bytes=fs.readFileSync(file);values.push([name,bytes.length,createHash('sha256').update(bytes).digest('hex')]);}}}scan(root);values.sort((a,b)=>a[0]<b[0]?-1:a[0]>b[0]?1:0);process.stdout.write(JSON.stringify(values));
}
