import {spawn,spawnSync} from 'node:child_process';
import {parseArgs} from 'node:util';
import * as config from './test-config.mjs';

function integrationGroups(groups){
  const entries=[...groups];
  const first=entries[0]?.[1];
  if(entries.length<2 || entries.some(([,group])=>group.library || group.testThreads!==first.testThreads))
    return entries;
  // Target names apply to every selected package. Check the real manifests before
  // combining them, so a same-named, unselected target cannot join the execution.
  const result=spawnSync(process.env.IVY_CARGO??'cargo',['metadata','--locked','--no-deps','--format-version','1'],
    {encoding:'utf8',windowsHide:true,timeout:30000,maxBuffer:16*1024*1024});
  if(result.error || result.status!==0)throw result.error??Error(result.stderr || 'Cannot read Cargo test targets.');
  const metadata=JSON.parse(result.stdout);
  const tests=new Set(entries.flatMap(([,group])=>[...group.tests]));
  for(const [name,group] of entries){
    const pkg=metadata.packages.find(pkg=>pkg.name===name);
    if(!pkg)throw Error('Cargo test package is not registered: '+name);
    if(pkg.targets.some(target=>target.kind.includes('test') && tests.has(target.name) && !group.tests.has(target.name)))
      return entries;
  }
  return [[entries.map(([pkg])=>pkg),{tests,testThreads:first.testThreads}]];
}

async function runCommands(commands){
  for(const args of commands){
    const child=spawn(process.env.IVY_CARGO??'cargo',args,{stdio:'inherit',windowsHide:true});
    const code=await new Promise((resolve,reject)=>{child.on('error',reject);child.on('close',resolve);});
    if(code!==0){process.exitCode=code??1;return false;}
  }
  return true;
}

export async function main(args=process.argv.slice(2)){
const {rustTests}=config;
const {values,positionals}=parseArgs({args,allowPositionals:true,options:{ignored:{type:'boolean'}}});
const groups=new Map();
const libraries=new Map();
const integrations=new Map();
const prerequisites=new Map();
for(const file of positionals){
  const target=rustTests[file];
  if(!target)throw Error('Unknown Cargo test target: '+file);
  if(values.ignored && (!target.lib || !target.filter))throw Error('--ignored requires filtered Cargo library tests.');
  if(target.platform && target.platform!==process.platform)
    throw Error('Cargo test unavailable on '+process.platform+'; requires '+target.platform+': '+file);
  if(target.platforms && !target.platforms.includes(process.platform))
    throw Error('Cargo test unavailable on '+process.platform+'; requires '+target.platforms.join('/')+': '+file);
  if(target.architectures && !target.architectures.includes(process.arch))
    throw Error('Cargo test unavailable on '+process.arch+'; requires '+target.architectures.join('/')+': '+file);
  if(target.test && target.filter){
    if(target.lib)throw Error('Filtered Cargo targets must select either a library or an integration test.');
    const key=JSON.stringify([target.package,target.test,target.filter]);
    const prior=integrations.get(key);
    if(prior && prior.testThreads!==target.testThreads)
      throw Error('Conflicting Cargo integration-test thread counts: '+target.package+'/'+target.test+'/'+target.filter);
    integrations.set(key,target);
  }else if(target.lib && target.filter){
    libraries.set(target.package+'/'+target.filter,target);
  }else{
    const group=groups.get(target.package)??{library:false,tests:new Set()};
    if(target.testThreads!==undefined){
      if(group.testThreads!==undefined && group.testThreads!==target.testThreads)
        throw Error('Conflicting Cargo package-test thread counts: '+target.package);
      group.testThreads=target.testThreads;
    }
    if(target.lib)group.library=true;
    if(target.test)group.tests.add(target.test);
    groups.set(target.package,group);
  }
  for(const required of target.prerequisites??[]){
    if(required.platform && required.platform!==process.platform)continue;
    prerequisites.set(required.package+'/'+required.bin,required);
  }
}
if(!groups.size && !libraries.size && !integrations.size)throw Error('Select at least one Cargo test file.');
function retainThreads(covering,target){
  if(target.testThreads===undefined)return;
  if(covering.testThreads!==undefined && covering.testThreads!==target.testThreads)
    throw Error('Conflicting Cargo test thread counts: '+target.package);
  covering.testThreads=target.testThreads;
}
const filtered=[...integrations.values()].filter(target=>{
  const group=groups.get(target.package);
  if(!group?.tests.has(target.test))return true;
  retainThreads(group,target);
  return false;
});
for(const [index,target] of filtered.entries()){
  if(filtered.slice(0,index).some(prior=>prior.package===target.package && prior.test===target.test &&
      (prior.filter.includes(target.filter) || target.filter.includes(prior.filter))))
    throw Error('Overlapping Cargo integration-test filters: '+target.package+'/'+target.test);
}
for(const target of libraries.values()){
  const group=groups.get(target.package);
  if(group?.library)retainThreads(group,target);
}
const testedPackages=positionals.map(file=>rustTests[file].package);
// Prerequisites include the selected packages. Compatible integration groups can
// share a Cargo invocation without replacing its working directories or runtime setup.
const required=[...prerequisites.values()];
if(required.length && !await runCommands([['build','--locked',
  ...[...new Set([...required.map(target=>target.package),...testedPackages])].flatMap(pkg=>['--package',pkg]),
  ...[...new Set(required.map(target=>target.bin))].flatMap(bin=>['--bin',bin])]]))return;
const commands=[
  ...integrationGroups(groups).map(([pkg,{library,tests,testThreads}])=>['test','--locked',
    ...[pkg].flat().flatMap(name=>['--package',name]),...(library?['--lib']:[]),...[...tests].flatMap(t=>['--test',t]),
    ...(testThreads!==undefined?['--','--test-threads='+testThreads]:[])]),
  ...[...libraries.values()].filter(target=>!groups.get(target.package)?.library)
    .map(target=>['test','--locked','--package',target.package,'--lib','--',target.filter,
      ...(values.ignored?['--ignored']:[]),...(target.testThreads?['--test-threads='+target.testThreads]:[])]),
  ...filtered.map(target=>['test','--locked','--package',target.package,'--test',target.test,'--',target.filter,
    ...(target.testThreads?['--test-threads='+target.testThreads]:[])]),
];
await runCommands(commands);
}
if(import.meta.main)await main();
