import {readFileSync,mkdirSync,writeFileSync} from 'node:fs';
import {stat} from 'node:fs/promises';
import {join} from 'node:path';
import {execFile,execFileSync} from 'node:child_process';
import {promisify} from 'node:util';
import vm from 'node:vm';
import ts from 'typescript';
let bytes='';for await(const chunk of process.stdin)bytes+=chunk;const input=JSON.parse(bytes);
const git=(...args)=>execFileSync('git',args,{windowsHide:true,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
if(input.action==='prepare'){
  const {root,variant}=input,repo=join(root,'repo'),origin=join(root,'origin.git');mkdirSync(repo,{recursive:true});
  if(variant==='file')writeFileSync(join(repo,'plain'),'plain');
  if(!['empty','missing','file'].includes(variant)){
    git('init','-b','main',repo);
    if(variant==='unborn'){console.log(JSON.stringify({path:repo,verifyOrigin:false}));process.exit(0);}
    writeFileSync(join(repo,'file.txt'),'one');git('-C',repo,'add','file.txt');
    const commit=()=>git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','-C',repo,'commit','-m','fixture');commit();
    if(variant==='dirty')writeFileSync(join(repo,'untracked'),'dirty');
    if(variant==='detached')git('-C',repo,'checkout','--detach');
    if(variant==='credentials')git('-C',repo,'remote','add','origin','https://fixture:secret@example.invalid/path.git');
    if(['confirmed','local-only','ancestor','missing-ref','unavailable','detached-origin'].includes(variant)){
      git('init','--bare','-b','main',origin);git('-C',repo,'remote','add','origin',origin);git('-C',repo,'push','origin','main');
      if(variant==='local-only'||variant==='ancestor'){const first=git('-C',repo,'rev-parse','HEAD');writeFileSync(join(repo,'file.txt'),'two');git('-C',repo,'add','file.txt');commit();if(variant==='ancestor'){git('-C',repo,'push','origin','main');git('-C',repo,'checkout','--detach',first);}}
      if(variant==='missing-ref')git('-C',repo,'checkout','-b','local-branch');
      if(variant==='unavailable')git('-C',repo,'remote','set-url','origin',join(root,'missing-origin.git'));
      if(variant==='detached-origin')git('-C',repo,'checkout','--detach');
    }
  }
  console.log(JSON.stringify({path:variant==='missing'?join(repo,'missing'):variant==='file'?join(repo,'plain'):repo,verifyOrigin:!['empty','missing','file','clean','dirty','detached','credentials'].includes(variant)}));
}else if(input.action==='redactions'){
  console.log(JSON.stringify(input.urls.map(value=>{try{const url=new URL(value);url.username='';url.password='';return {input:value,result:url.toString()};}catch{return {input:value,error:true};}})));
}else{
  const source=readFileSync('services/agent-manager/src/projects.ts','utf8'),file=ts.createSourceFile('projects.ts',source,ts.ScriptTarget.Latest,true);let expression,workspaceBody;
  function visit(node){if(ts.isVariableDeclaration(node)&&node.name.getText(file)==='inspect')expression=node.initializer.getText(file);if(ts.isMethodDeclaration(node)&&node.name.getText(file)==='workspace')workspaceBody=node.body;ts.forEachChild(node,visit);}visit(file);if(!expression)throw Error('Actual inspect function missing');
  const RealDate=Date;class FixedDate extends RealDate{constructor(...args){super(...(args.length?args:['2026-10-18T00:00:00.000Z']));}}
  const context={stat,promisify,execFile,URL,Date:FixedDate,verifyOrigin:input.verifyOrigin,intendedPath:input.path,sourcePath:input.source??null,useWorktree:input.useWorktree};
  const inspect=vm.runInNewContext(ts.transpile(`const inspect=${expression}; inspect;`,{target:ts.ScriptTarget.ES2022}),context);
  if(input.action==='inspectWorkspace'){
    const statements=workspaceBody.statements,first=statements.findIndex(node=>ts.isVariableStatement(node)&&node.declarationList.declarations.some(declaration=>declaration.name.getText(file)==='repository')),last=statements.findIndex(node=>ts.isVariableStatement(node)&&node.declarationList.declarations.some(declaration=>declaration.name.getText(file)==='nativeProjectId'));
    if(first<0||last<first)throw Error('Actual workspace observation missing');
    const fragment=statements.slice(first,last).map(node=>node.getText(file)).join('\n');context.inspect=inspect;
    console.log(JSON.stringify(await vm.runInNewContext(ts.transpile(`(async()=>{${fragment};return repository;})();`,{target:ts.ScriptTarget.ES2022}),context)));
  }else console.log(JSON.stringify(await inspect(input.path)));
}
