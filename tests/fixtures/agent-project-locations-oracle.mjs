import {readFileSync,mkdirSync,writeFileSync,renameSync,symlinkSync} from 'node:fs';
import {mkdir,realpath,stat,access} from 'node:fs/promises';
import {constants} from 'node:fs';
import * as path from 'node:path';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import ts from 'typescript';
import {inside} from '../../dist/packages/host-runtime/src/config.js';
import {resolvedFuturePath} from '../../dist/packages/host-runtime/src/layout.js';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
import {LocalAgentState} from '../../dist/services/agent-manager/src/hive-state.js';
import {validateAgent} from '../../dist/packages/contracts/src/agent-validation.js';
import {listDirectories} from '../../dist/services/agent-manager/src/directories.js';

let bytes='';for await(const chunk of process.stdin)bytes+=chunk;const input=JSON.parse(bytes);
const resolve=(selection,expected)=>({action:'resolve',selection:['normal','internal','task'].includes(selection.kind)?{key:'fixture',name:'Fixture',...selection}:selection,...(expected===undefined?{}:{expected})});
const workspace=(requirement,prepare=false,extra={})=>({action:'workspace',taskKey:'TASK-0012',requirement,prepare,...extra});
const project=(id,p)=>({nativeId:id,name:id,paths:[p],source:'native'});
const internal=project('internal','<root>/internal');
const selection={kind:'normal',key:'fixture',name:'Created'};
const cases=[
  {name:'projectless-principal-and-key-isolation',steps:[resolve({kind:'projectless',key:'one'}),resolve({kind:'projectless',key:'one'}),{...resolve({kind:'projectless',key:'one'}),principal:'other'},resolve({kind:'projectless',key:'two'}),resolve({kind:'projectless',key:'one'},'internal')]},
  {name:'internal-task-selection-and-guard-order',steps:[resolve({kind:'internal'}),resolve({kind:'task',key:'TASK-0012'}),resolve({kind:'task',key:'../escape'}),resolve({kind:'internal'},'different'),{action:'close'},resolve({kind:'task',key:'../escape'})]},
  {name:'first-internal-identity-wins',projects:[internal,project('duplicate','<root>/internal')],steps:[resolve({kind:'internal'},'duplicate'),resolve({kind:'internal'},'internal')]},
  {name:'new-project-retains-allocation-and-registration',steps:[resolve(selection),resolve(selection),{action:'state',selection},{action:'restart'},resolve(selection),{action:'state',selection}]},
  {name:'existing-directory-is-not-new-allocation',directories:['projects/Created'],steps:[resolve(selection),{action:'state',selection}]},
  {name:'replacement-retains-original-identity',steps:[resolve(selection),{action:'fs',kind:'replace',path:'projects/Created',target:'projects/original'},resolve(selection),{action:'state',selection}]},
  {name:'redirected-directory-is-not-original-path',steps:[resolve(selection),{action:'fs',kind:'redirect',path:'projects/Created',target:'projects/original'},resolve(selection),{action:'state',selection}]},
  {name:'invalid-new-directory-names',steps:['..','a/b','a\\b','bad\u0000name',' ','normal'].map(name=>resolve({kind:'normal',name}))},
  {name:'normal-project-cannot-enter-internal-root',settings:{internalProjectRoot:'<root>/projects'},projects:[project('internal','<root>/projects')],steps:[resolve(selection)]},
  {name:'protected-root-prevents-projectless-and-internal',protectedRoots:['<root>/internal'],steps:[resolve({kind:'projectless',key:'one'}),resolve({kind:'internal'})]},
  {name:'allocation-state-unavailable',state:false,steps:[resolve(selection)]},
  {name:'registration-unavailable-after-allocation',register:false,steps:[resolve(selection),resolve(selection),{action:'state',selection}]},
  {name:'registration-identity-conflict-retains-allocation',wrongRegistration:true,steps:[resolve(selection),{action:'state',selection}]},
  {name:'existing-project-and-expected-identity',projects:[internal,project('known','<root>/existing')],steps:[resolve({kind:'existing',cwd:'<root>/existing'},'known'),resolve({kind:'existing',cwd:'<root>/existing'},'missing'),resolve({kind:'existing',cwd:'relative'}),resolve({kind:'existing',cwd:'<root>/file.txt'}),resolve({kind:'existing',cwd:'<root>/missing'}),resolve({kind:'existing',cwd:'<root>/other'})]},
  {name:'task-and-new-workspace-preparation',steps:[workspace({kind:'task_workspace'}),workspace({kind:'task_workspace'},true),workspace({kind:'new_project_path',folderName:'Workspace'}),workspace({kind:'new_project_path',folderName:'Workspace'},true),workspace({kind:'repository_path',folderName:'Workspace'},true),workspace({kind:'new_project_path',folderName:'../escape'},true)]},
  {name:'absolute-directory-workspaces',steps:[workspace({kind:'directory_path',path:'<root>/existing'}),workspace({kind:'directory_path',path:'<root>/file.txt'}),workspace({kind:'directory_path',path:'<root>/missing'}),workspace({kind:'directory_path',path:'relative'}),workspace({kind:'task_workspace'},false,{taskKey:'TASK-001'})]},
  {name:'existing-workspace-selected-path-guards',projects:[internal,project('known','<root>/existing')],steps:[workspace({kind:'existing_project',projectId:'missing',useWorktree:false}),workspace({kind:'existing_project',projectId:'known',path:'<root>/other',useWorktree:false}),workspace({kind:'existing_project',projectId:'known',useWorktree:false}),workspace({kind:'existing_project',projectId:'known',useWorktree:true}),workspace({kind:'existing_project',projectId:'known',useWorktree:true},true)]},
];
const handlerCases=[];
const addHandler=(name,args,extra={})=>handlerCases.push({name:name+'-'+handlerCases.length,steps:[{action:'handler',name,args,context:{callerPrincipalId:'caller'},...extra}]});
for(const name of ['resolveProject','resolveWorkspace','listDirectories'])for(const args of [null,{},7])addHandler(name,args,{available:false});
for(const available of [false,true]){
  addHandler('resolveProject',{selection:{kind:'projectless',key:'one'}},{available,context:{callerPrincipalId:'other',cancelled:true}});
  addHandler('resolveProject',{selection:{kind:'internal',key:'fixture',name:'Fixture'}},{available});
  addHandler('listDirectories',{path:'relative'},{available});
}
for(const freshError of [false,true])for(const operationId of [null,'','operation'])addHandler('resolveWorkspace',{taskKey:'TASK-0012',requirement:{kind:'task_workspace'},prepare:true},{freshError,context:{callerPrincipalId:'caller',operationId,cancelled:true}});
addHandler('resolveWorkspace',{taskKey:'TASK-0012',requirement:{kind:'task_workspace'},prepare:false});
addHandler('resolveProject',{selection:{kind:'projectless',key:'one'},expectedProjectId:'internal'});
addHandler('resolveProject',{selection:{kind:'internal',key:'fixture',name:'Fixture'},expectedProjectId:'different'},{freshError:true});
function expand(value,root){if(typeof value==='string')return value.replaceAll('<root>',root);if(Array.isArray(value))return value.map(v=>expand(v,root));if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,expand(v,root)]));return value;}
function normalize(value,root){if(typeof value==='string')return value.replaceAll(path.toNamespacedPath(root),'<root>').replaceAll(root,'<root>').replaceAll('\\','/');if(Array.isArray(value))return value.map(v=>normalize(v,root));if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v,root)]));return value;}
function target(root,value){const selected=path.resolve(root,value);if(!inside(root,selected)||selected===root)throw Error('Fixture escaped its owned root');return selected;}
function prepare(root,row){mkdirSync(root,{recursive:true});for(const name of ['data','projects','internal','existing','other',...(row.directories??[])])mkdirSync(target(root,name),{recursive:true});writeFileSync(target(root,'file.txt'),'file');}
function mutate(root,step){const source=target(root,step.path),destination=target(root,step.target);renameSync(source,destination);if(step.kind==='redirect')symlinkSync(destination,source,process.platform==='win32'?'junction':'dir');else mkdirSync(source);}
function allocation(state,root,request){return (async()=>{const key=hashJson({principal:request.principal??'caller',selection:request.selection}),saved=await state?.read('project-allocation',key);if(!saved)return null;let identityMatches=false;try{const info=await stat(saved.value.cwd,{bigint:true});identityMatches=info.dev.toString()===saved.value.dev&&info.ino.toString()===saved.value.ino;}catch{}return {revision:saved.pin.revision,path:saved.value.cwd,identityMatches};})();}
if(input.action==='catalog')console.log(JSON.stringify(cases));
else if(input.action==='handlerCatalog')console.log(JSON.stringify(handlerCases));
else if(input.action==='prepare'){prepare(input.root,input.case);console.log('null');}
else if(input.action==='mutate'){mutate(input.root,input.step);console.log('null');}
else if(input.action==='readAllocation'){
  const state=new LocalAgentState(path.join(input.root,'state.sqlite'),'agent',{maxOperations:1000,maxJournalBytes:128*1024*1024});
  try{console.log(JSON.stringify(normalize(await allocation(state,input.root,expand(input.step,input.root)),input.root)));}finally{state.close();}
}else if(input.action==='normalize')console.log(JSON.stringify(normalize(input.value,input.root)));
else{
  const root=input.root,row=expand(input.case,root);prepare(root,row);
  const source=readFileSync('services/agent-manager/src/projects.ts','utf8'),file=ts.createSourceFile('projects.ts',source,ts.ScriptTarget.Latest,true);let classText;
  function visit(node){if(ts.isClassDeclaration(node)&&node.name?.text==='ProjectLocations')classText=node.getText(file).replace(/^export\s+/,'');ts.forEachChild(node,visit);}visit(file);
  // Use this realm: canonical() intentionally rejects objects from another VM
  // realm, which is unrelated to the original project allocation behavior.
  const dependencies={mkdir,realpath,stat,access,constants,execFile,promisify,basename:path.basename,isAbsolute:path.isAbsolute,join:path.join,resolve:path.resolve,inside,resolvedFuturePath,accountHome:()=>root,IvyError,requireThat,hashJson,structuredClone,process,URL};
  const Locations=Function(...Object.keys(dependencies),ts.transpile(classText,{target:ts.ScriptTarget.ES2022})+'\nreturn ProjectLocations;')(...Object.values(dependencies));
  const settings={projectRoot:path.join(root,'projects'),internalProjectRoot:path.join(root,'internal'),...row.settings},trace=[],calls=[],effects=[];
  let state=row.state===false?undefined:new LocalAgentState(path.join(root,'state.sqlite'),'agent',{maxOperations:1000,maxJournalBytes:128*1024*1024}),locations,projects=structuredClone(row.projects??expand([internal],root));
  const open=()=>{locations=new Locations(path.join(root,'data'),settings,row.protectedRoots??[],row.register===false?undefined:async request=>{calls.push({name:request.name,cwd:request.cwd,idempotencyValid:request.idempotencyKey===hashJson({principal:current.principal??'caller',selection:current.selection})});const value=project('registered',row.wrongRegistration?path.join(root,'other'):request.cwd);projects.push(value);locations.setNativeProjects(projects);return value;},state);locations.setNativeProjects(projects);};
  let current;open();
  const handlers={};
  if(input.action==='handlers'){
    const mainSource=readFileSync('services/agent-manager/src/main.ts','utf8'),main=ts.createSourceFile('main.ts',mainSource,ts.ScriptTarget.Latest,true),expressions={};
    function find(node){if(ts.isPropertyAssignment(node)&&ts.isStringLiteral(node.name)&&['agent.resolveProject','agent.resolveWorkspace','agent.listDirectories'].includes(node.name.text))expressions[node.name.text.slice(6)]=node.initializer.getText(main);ts.forEachChild(node,find);}find(main);
    const available=()=>{effects.push('available');requireThat(current.available!==false,'native_unavailable','The current native owner is unavailable.');};
    const freshProjects=async()=>{effects.push('fresh');if(current.freshError)throw new IvyError('fixture_fresh_failed','Fixture project refresh failed.');};
    const handlerDependencies={validateAgent,available,freshProjects,service:{connection:{}},locations,config:{hostId:'host',serviceNodeId:'agent'},listDirectories,requireThat,track:work=>work};
    for(const [name,expression] of Object.entries(expressions))handlers[name]=Function(...Object.keys(handlerDependencies),ts.transpile('const handler='+expression+';',{target:ts.ScriptTarget.ES2022})+'return handler;')(...Object.values(handlerDependencies));
  }
  try{for(current of row.steps){try{let value;
    if(current.action==='resolve')value=await locations.resolve(current.selection,current.principal??'caller',current.expected);
    else if(current.action==='workspace')value=await locations.workspace(current.taskKey,current.requirement,current.prepare,'host','agent',current.verifyOrigin??false);
    else if(current.action==='handler')value=await handlers[current.name](current.args,{...current.context,signal:current.context.cancelled?AbortSignal.abort(new IvyError('fixture_cancelled','Fixture caller cancelled.')):new AbortController().signal});
    else if(current.action==='state')value=await allocation(state,root,current);
    else if(current.action==='fs'){mutate(root,current);value=null;}
    else if(current.action==='close'){locations.close();projects=[];value=null;}
    else if(current.action==='restart'){locations.close();state.close();state=new LocalAgentState(path.join(root,'state.sqlite'),'agent',{maxOperations:1000,maxJournalBytes:128*1024*1024});open();value=null;}
    trace.push({result:value});
  }catch(error){trace.push({error:IvyError.from(error).toWire()});}}console.log(JSON.stringify(normalize({trace,calls,...(input.action==='handlers'?{effects}:{})},root)));}finally{locations.close();state?.close();}
}
