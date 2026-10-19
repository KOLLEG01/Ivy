import * as fs from 'node:fs/promises';
import {getSystemErrorMap} from 'node:util';
let text='';
for await (const chunk of process.stdin) text+=chunk;
const input=JSON.parse(text);
let value;
if(input.mapping){
  const pair=[...getSystemErrorMap()].find(([, [code]])=>code===input.mapping);
  if(!pair)throw Error('Missing libuv mapping');
  value={code:pair[1][0],errno:pair[0],description:pair[1][1]};
}else{
  const operation={scandir:'readdir',stat:'stat',rmdir:'rmdir',unlink:'unlink',lstat:'lstat'}[input.syscall];
  if(!operation)throw Error('Unsupported fixture operation');
  try{
    await fs[operation](input.path);
    throw Error('Fixture must fail');
  }catch(error){
    if(typeof error.code!=='string'||typeof error.errno!=='number')throw error;
    value={code:error.code,errno:error.errno,syscall:error.syscall,path:error.path,message:error.message};
  }
}
process.stdout.write(JSON.stringify({runtime:{node:process.versions.node,uv:process.versions.uv,platform:process.platform,arch:process.arch},value}));
