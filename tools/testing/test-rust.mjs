import {spawn} from 'node:child_process';
import {rustTests} from './test-config.mjs';
const groups=new Map();
const prerequisites=new Map();
for(const file of process.argv.slice(2)){
  const target=rustTests[file];
  if(!target)throw Error('Unknown Cargo test target: '+file);
  const group=groups.get(target.package)??{library:false,tests:new Set()};
  if(target.lib)group.library=true;
  if(target.test)group.tests.add(target.test);
  groups.set(target.package,group);
  for(const required of target.prerequisites??[]){
    if(required.platform && required.platform!==process.platform)continue;
    prerequisites.set(required.package+'/'+required.bin,required);
  }
}
if(!groups.size)throw Error('Select at least one Cargo test file.');
const commands=[
  ...[...prerequisites.values()].map(required=>['build','--locked','--package',required.package,'--bin',required.bin]),
  ...[...groups].map(([pkg,{library,tests}])=>['test','--locked','--package',pkg,...(library?['--lib']:[]),...[...tests].flatMap(t=>['--test',t])]),
];
for(const args of commands){
  const child=spawn(process.env.IVY_CARGO??'cargo',args,{stdio:'inherit',windowsHide:true});
  const code=await new Promise((resolve,reject)=>{child.on('error',reject);child.on('close',resolve);});
  if(code!==0){process.exitCode=code??1;break;}
}
