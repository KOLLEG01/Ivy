import {spawn} from 'node:child_process';
import {rustTests} from './test-config.mjs';
const groups=new Map();
const prerequisites=new Map();
for(const file of process.argv.slice(2)){
  const target=rustTests[file];
  if(!target)throw Error('Unknown Cargo test target: '+file);
  const tests=groups.get(target.package)??new Set();tests.add(target.test);groups.set(target.package,tests);
  for(const required of target.prerequisites??[]){
    if(required.platform && required.platform!==process.platform)continue;
    prerequisites.set(required.package+'/'+required.bin,required);
  }
}
if(!groups.size)throw Error('Select at least one Cargo test file.');
const commands=[
  ...[...prerequisites.values()].map(required=>['build','--locked','--package',required.package,'--bin',required.bin]),
  ...[...groups].map(([pkg,tests])=>['test','--locked','--package',pkg,...[...tests].flatMap(t=>['--test',t])]),
];
for(const args of commands){
  const child=spawn(process.env.IVY_CARGO??'cargo',args,{stdio:'inherit',windowsHide:true});
  const code=await new Promise((resolve,reject)=>{child.on('error',reject);child.on('close',resolve);});
  if(code!==0){process.exitCode=code??1;break;}
}
