import {rmSync} from 'node:fs';
let text='';
for await(const chunk of process.stdin)text+=chunk;
const input=JSON.parse(text);
let result;
try{
  rmSync(input.path,{recursive:true,force:true});
  result={ok:true,error:null};
}catch(error){
  result={ok:false,error:{code:error.code,errno:error.errno,syscall:error.syscall,path:error.path,message:error.message}};
}
process.stdout.write(JSON.stringify({runtime:{node:process.versions.node,uv:process.versions.uv,platform:process.platform,arch:process.arch},result}));
