import {spawn} from 'node:child_process';
import {rustTests} from './test-config.mjs';
const groups=new Map();
for(const file of process.argv.slice(2)){
  const target=rustTests[file];
  if(!target)throw Error('Unknown Cargo test target: '+file);
  const tests=groups.get(target.package)??new Set();tests.add(target.test);groups.set(target.package,tests);
}
if(!groups.size)throw Error('Select at least one Cargo test file.');
for(const [pkg,tests] of groups){
  const child=spawn(process.env.IVY_CARGO??'cargo',['test','--locked','--package',pkg,...[...tests].flatMap(t=>['--test',t])],{stdio:'inherit',windowsHide:true});
  const code=await new Promise((resolve,reject)=>{child.on('error',reject);child.on('close',resolve);});
  if(code!==0){process.exitCode=code??1;break;}
}
