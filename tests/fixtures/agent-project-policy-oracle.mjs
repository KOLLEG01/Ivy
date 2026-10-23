import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import path from 'node:path';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
const source=readFileSync('services/agent-manager/src/projects.ts','utf8'),file=ts.createSourceFile('projects.ts',source,ts.ScriptTarget.Latest,true);
const expressions={},guards={};let classText;
function visit(node){
  if(ts.isClassDeclaration(node)&&node.name?.text==='ProjectLocations')classText=node.getText(file).replace(/^export\s+/,'');
  if(ts.isVariableDeclaration(node)&&['nativeProjectId','allocationKey'].includes(node.name.getText(file)))expressions[node.name.getText(file)]=node.initializer.getText(file);
  if(ts.isVariableDeclaration(node)&&['project','existing'].includes(node.name.getText(file))&&node.initializer?.getText(file).startsWith('this.nativeProjects.find'))expressions[node.name.getText(file)]=node.initializer.getText(file);
  if(ts.isCallExpression(node)&&node.expression.getText(file)==='requireThat'&&node.arguments[2]&&ts.isStringLiteral(node.arguments[2]))guards[node.arguments[2].text]={predicate:node.arguments[0].getText(file),code:node.arguments[1].text,message:node.arguments[2].text};
  ts.forEachChild(node,visit);
}visit(file);
const cfg=readFileSync('packages/host-runtime/src/config.ts','utf8'),cfgFile=ts.createSourceFile('config.ts',cfg,ts.ScriptTarget.Latest,true);
let insideText;function findInside(node){if(ts.isFunctionDeclaration(node)&&node.name?.text==='inside')insideText=node.getText(cfgFile).replace(/^export\s+/,'');ts.forEachChild(node,findInside);}findInside(cfgFile);
const transpile=text=>ts.transpile(text,{target:ts.ScriptTarget.ES2022});
const cases=[];
for(const style of ['win32','posix']){
  const paths=path[style],cwd=style==='win32'?'C:\\work':'/work',home=style==='win32'?'C:\\user':'/user',data=style==='win32'?'C:\\data':'/data';
  const base={style,cwd,home,data};
  const context={structuredClone,hashJson,IvyError,requireThat,process:{platform:style==='win32'?'win32':'linux'},accountHome:()=>home,...paths,resolve:(...values)=>paths.resolve(cwd,...values)};
  vm.createContext(context);vm.runInContext(transpile(insideText),context);
  const Locations=vm.runInContext(transpile(classText)+'\nProjectLocations;',context);
  const capture=work=>{try{return {result:work()};}catch(error){return {error:IvyError.from(error).toWire()};}};
  const add=(action,input,work)=>cases.push({...base,action,input,expected:capture(work)});
  for(const settings of [{},{projectRoot:null,internalProjectRoot:null},{projectRoot:'relative'},{internalProjectRoot:''},{projectRoot:17},{projectRoot:home,internalProjectRoot:cwd}])add('defaults',{settings},()=>new Locations(data,settings).defaults);
  const policy=new Locations(data,{});
  for(const left of [cwd,home,paths.join(cwd,'a'),paths.join(cwd,'a/..'),paths.join(cwd,'ΙΣ'),paths.join(cwd,'İ'),paths.join(cwd,'x\ud800')])for(const right of [left,left.toUpperCase(),left.replaceAll('\\','/'),paths.join(left,'child'),'relative'])add('samePath',{left,right},()=>policy.samePath(left,right));
  const guard=(label,selection,extras={})=>{
    const row=guards[label];if(!row)throw Error(label);
    return ()=>{context.selection=selection;context.taskKey=extras.taskKey;context.requirement=extras.requirement;requireThat(vm.runInContext(transpile('('+row.predicate+');'),context),row.code,row.message);return null;};
  };
  for(const key of ['a','TASK-0001','.','..','../escape','child/name','child\\name','_first','a_b-c.d','A'.repeat(128),'A'.repeat(129),'é','x\ud800',''])add('taskKey',{key},guard('Task directory key must be one safe relative directory name.',{key}));
  for(const key of ['TASK-0001','TASK-12345','TASK-001','task-0001','TASK-１２３４','TASK-0001x','TASK-'+'0'.repeat(300),''])add('workspaceKey',{key},guard('Task workspace requires its immutable Task key.',{}, {taskKey:key}));
  for(const name of ['normal',' name','name ','name.','.','..',' ','\uFEFF','\u0085','é','😀','CON','con.txt','COM0','COM1.x','COM9','COM10','LPT0','LPT9.txt','a/b','a\\b','a\u0000b','a:b','a?b','x\ud800'])add('projectName',{name},guard('Project name must be one valid directory name on this host.',{name}));
  for(const name of ['','normal','.','..','a/b','a\\b','a:b','\u0000','name.'])add('workspaceFolder',{name},guard('Project folder must be one relative directory name.',{}, {requirement:{folderName:name}}));
  for(const principal of ['caller','caller\ud800','other'])for(const key of ['task','TASK-0001','key😀','']){
    const selection={kind:'projectless',key,name:'ignored'};
    add('allocationKey',{principal,selection},()=>hashJson({principal,selection}));
    add('projectlessPath',{principal,key},()=>paths.join(policy.defaults.internalProjectRoot,'unassigned',hashJson({principal,key}).slice(7,39)));
  }
  const projects=[{nativeId:'first',paths:[paths.join(cwd,'nested')],name:'A',source:'native'},{nativeId:'tie',paths:[paths.join(cwd,'nested')],name:'B',source:'native'},{nativeId:'outer',paths:[cwd],name:'C',source:'native'},{nativeId:'deep',paths:[paths.join(cwd,'nested/deep')],name:'D',source:'native'}];
  const matching={nativeId:'internal',paths:[policy.defaults.internalProjectRoot],name:'Internal',source:'native'},duplicate={...matching,nativeId:'duplicate'};
  for(const entries of [[],[matching],[duplicate,matching],[matching,duplicate],[...projects,matching]])for(const expectedProjectId of [null,'','internal','missing'])for(const action of ['internalProject','existingProject']){
    policy.setNativeProjects(entries);context.subject=policy;context.root=policy.defaults.internalProjectRoot;context.cwd=context.root;context.expectedProjectId=expectedProjectId??undefined;
    const local=action==='internalProject'?'project':'existing',label=action==='internalProject'?'Register the internal working directory as a Codex project before starting service tasks.':'The selected Codex project no longer belongs to this working directory.',row=guards[label];
    add(action,{projects:entries,cwd:context.cwd,expectedProjectId},()=>vm.runInContext(transpile(`function value(){const ${local}=${expressions[local]}; requireThat(${row.predicate},${JSON.stringify(row.code)},${JSON.stringify(row.message)});return ${local}??null;} value.call(subject);`),context));
  }
  policy.setNativeProjects(projects);
  const evaluate=()=>vm.runInContext(transpile(`function value(){return ${expressions.nativeProjectId};}; value.call(subject);`),context);
  for(const canonicalCwd of [cwd,paths.join(cwd,'child'),paths.join(cwd,'nested'),paths.join(cwd,'nested/deep/child'),home])for(const explicit of [null,'selected','']){
    context.canonicalCwd=canonicalCwd;context.project=explicit===null?null:{nativeId:explicit};context.subject=policy;
    add('workspaceProject',{projects,cwd:canonicalCwd,explicit},()=>evaluate()??null);
  }
}
console.log(JSON.stringify({cases,guards,expressions}));
