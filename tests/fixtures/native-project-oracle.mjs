import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {defaultNativeThreadProject,nativeThreadProject}=await import('../../packages/sdk/src/native-project.ts');
const {normalizedProjectPath,nativeProjectForThread,isInternalProject}=await import('../../packages/sdk/src/native-project-membership.ts');
const rows=[];
const capture=fn=>{try{return {ok:true,value:fn()??null};}catch(e){return {ok:false,code:e.code??null,message:e.message};}};
const add=(kind,input,fn)=>rows.push({kind,input:JSON.stringify(input),expected:JSON.stringify(capture(fn))});
const project=(id,paths,name=id)=>({nativeId:id,name,paths,source:'native'});
const inventory=(projects,internal)=>({source:'native',observedAt:'fixture',projects,...(internal===undefined?{}:{defaults:{projectRoot:'/projects',internalProjectRoot:internal}})});
const paths=['','/','//','///','/A','/A/','/A//B/../C','/A\\','/A/./../..','C:/','C:\\','C:\\Work\\Child','c:/work/child/..','C:relative','relative','\\relative','\\\\Server\\Share','\\\\Server\\Share\\Child','\\\\Server','\\\\Server\\','\\\\Server\\Share\\..\\Child','\\\\\\Server\\Share','\\\\.\\COM1:','\\\\.\\PhysicalDrive0','\\\\.\\COM¹:\\..\\child','\\\\?\\C:\\Work','\\\\?\\UNC\\Server\\Share\\A','\\\\?\\uNc\\?\\C:\\Work','//Server/Share/A','/work/../external','C:/ΟΣ/İ/Σ','C:/AΣ\ud800Σ','C:/\ud800/\udfff','/root/😀','/root/𐐀','C:/\u{16EA0}'];
let seed=71285;const random=()=>{seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed;};
const roots=['/','//Server/Share/','C:/','D:\\','\\\\Server\\Share\\','\\\\?\\C:\\','\\\\.\\COM2:\\','relative/'];
const pieces=['a','B','..','.','','ΟΣ','İ','\ud800','😀','COM1:'];
for(let n=0;n<250;n++){let path=roots[random()%roots.length];for(let i=0;i<1+random()%7;i++)path+=pieces[random()%pieces.length]+['/','\\','//'][random()%3];paths.push(path);}
for(const path of paths){
  add('normalize',{path},()=>normalizedProjectPath(path));
  const projects=[project('root',['/','C:/']),project('work',['/work','C:/work']),project('long',['/work/sub','C:/work/sub']),project('unc',['\\\\Server\\Share']),project('unicode',['C:/ος','C:/aς\ud800σ']),project('first',[path]),project('tie',[path])];
  for(const thread of [{cwd:path},{cwd:path,projectId:''},{cwd:path,projectId:null},{cwd:path,projectId:'tie'},{cwd:path,projectId:'absent'},{cwd:path,projectId:false}])add('membership',{projects,thread},()=>nativeProjectForThread(projects,thread));
  const selected=inventory(projects.slice(0,5),'/internal');
  for(const params of [{cwd:path},{cwd:path,projectId:''},{cwd:path,ephemeral:true}])add('default',{params,inventory:selected},()=>defaultNativeThreadProject(params,selected));
  const exact=inventory([project('exact',[path])]);add('default',{params:{cwd:path},inventory:exact},()=>defaultNativeThreadProject({cwd:path},exact));
}
for(const params of [{},{cwd:null},{cwd:17},{projectId:0},{projectId:false},{projectId:{}},{projectId:null},{projectId:''},{ephemeral:true},{cwd:'/external'},{cwd:'/internal'},{cwd:'/internal/sub'}]){
  for(const selected of [inventory([]),inventory([],'/internal'),inventory([project('internal',['/internal'],'IvyInternal')],'/internal'),inventory([project('foreign',['relative'])],'/internal')])add('default',{params,inventory:selected},()=>defaultNativeThreadProject(params,selected));
}
for(const name of ['IvyInternal','ivyinternal','IvyInternal\ud800',''])add('internal',{project:project('test',[],name)},()=>isInternalProject(project('test',[],name)));
for(const projects of [[project('short',['/x']),project('long',['/x/😀'])],[project('first',['/x/😀']),project('second',['/x/😀'])],[project('empty',[''])]])add('membership',{projects,thread:{cwd:'/x/😀/z'}},()=>nativeProjectForThread(projects,{cwd:'/x/😀/z'}));
// All default Unicode lowercase mappings plus boundaries of contextual-casing properties.
const lowercase=[],edges=[],cased=/\p{Cased}/u,ignorable=/\p{Case_Ignorable}/u;let previous=-1;
for(let cp=0;cp<=0x10ffff;cp++){
  if(cp>=0xd800&&cp<=0xdfff)continue;
  const c=String.fromCodePoint(cp),lower=c.toLowerCase();if(c!==lower)lowercase.push([cp,lower]);
  const flags=(cased.test(c)?1:0)|(ignorable.test(c)?2:0);
  if(flags!==previous){for(const n of [cp-1,cp,cp+1])if(n>=0&&n<=0x10ffff)edges.push(n);previous=flags;}
}
for(const cp of new Set(edges))for(const path of ['C:/AΣ'+String.fromCodePoint(cp),'C:/A'+String.fromCodePoint(cp)+'Σ','C:/AΣ'+String.fromCodePoint(cp)+'A'])add('normalize',{path},()=>normalizedProjectPath(path));
for(const params of [{projectId:''},{projectId:'explicit'},{ephemeral:true},{cwd:'/work/sub'},{cwd:'/internal'}]){
  const selected=inventory([project('work',['/work'])],'/internal'),trace=[];
  const definition={namespace:'agent',name:'projects',interfaceVersion:'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}};
  const client={async request(method,value,options){trace.push({method,params:value,...(method==='tools.call'?{timeout:options.timeoutMs}: {})});
    if(method==='tools.list')return {provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:'agent.projects',definition,definitionHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null};
    if(method==='tools.call')return structuredClone(selected);throw Error('Unexpected request');}};
  let expected;try{expected={ok:true,value:await nativeThreadProject(client,'native-agent',params,{timeoutMs:1234})};}catch(e){expected={ok:false,code:e.code,message:e.message};}
  rows.push({kind:'rpc',input:JSON.stringify({params,inventory:selected}),expected:JSON.stringify(expected),trace:JSON.stringify(trace)});
}
process.stdout.write(JSON.stringify({unicode:process.versions.unicode,lowercase,rows}));
