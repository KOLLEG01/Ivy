import assert from 'node:assert/strict';
import {readFileSync,existsSync} from 'node:fs';
import {mkdir,writeFile,readFile,readdir,lstat,symlink} from 'node:fs/promises';
import {registerHooks,createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {createHash} from 'node:crypto';
const input=JSON.parse(readFileSync(0,'utf8')),boundary=path.resolve(input.fixtureRoot),root=path.resolve(input.root);
assert(path.basename(boundary).startsWith('ivy-config-'));assert.equal(path.dirname(root),boundary);
const setupPath=path.join(root,'reset-plan-setup.json'),ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript');
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {hashJson,digest}=await import('../../packages/contracts/src/canonical.ts');
const {atomicJson,inside}=await import('../../packages/host-runtime/src/config.ts');
const {hostConfig}=await import('../../packages/host-runtime/src/host-config.ts');
const {HostJournal}=await import('../../packages/host-runtime/src/journal.ts');
const {servicePaths,resolvedFuturePath}=await import('../../packages/host-runtime/src/layout.ts');
const {sourceBuildPlan}=await import('../../packages/host-runtime/src/source.ts');
const {releaseManifest}=await import('../../packages/host-runtime/src/artifact.ts');
const source=readFileSync(new URL('../../packages/host-runtime/src/runtime-reset.ts',import.meta.url),'utf8');
const ast=ts.createSourceFile('reset.ts',source,ts.ScriptTarget.ESNext,true),names=['exists','collectScopeRoots','configuredScopeRootIds','treeHash','deployment','componentManifest','areaRoot','localRuntimeResetPlan'];
const declarations=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.includes(n.name?.text)).map(n=>n.getText(ast).replace(/^export\s+/,''));assert.equal(declarations.length,names.length);
const body=ts.transpileModule(declarations.join('\n')+'\nreturn localRuntimeResetPlan;',{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
const scope={IvyError,requireThat,lstat,readdir,readFile,hashJson,digest,HostJournal,servicePaths,resolvedFuturePath,sourceBuildPlan,releaseManifest,resolve:path.resolve,isAbsolute:path.isAbsolute,join:path.join,inside};
const original=new Function(...Object.keys(scope),body)(...Object.values(scope));
const sha=value=>'sha256:'+createHash('sha256').update(value).digest('hex');
if(input.kind==='setup'){
 await mkdir(root,{recursive:true});
 const tests=readFileSync(new URL('../runtime-reset.test.ts',import.meta.url),'utf8'),testAst=ts.createSourceFile('test.ts',tests,ts.ScriptTarget.ESNext,true);
 const declaration=testAst.statements.find(n=>ts.isFunctionDeclaration(n)&&n.name?.text==='fixture').getText(testAst);
 const factoryBody=ts.transpileModule(declaration+'\nreturn fixture;',{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
 const factoryScope={mkdtemp:async()=>root,join:path.join,tmpdir:()=>root,atomicJson,hostConfig,servicePaths,mkdir,writeFile,rm:async()=>{}};
 const fixture=await new Function(...Object.keys(factoryScope),factoryBody)(...Object.values(factoryScope))({after(){}});
 const {config,configPath,distribution,paths}=fixture,planPath=path.join(distribution,'services/fixture/deploy.json'),component=JSON.parse(await readFile(planPath,'utf8'));
 if(input.case==='missing-layout')delete component.runtimeReset;
 if(input.case==='delete-escape')component.runtimeReset.delete[0].path='../escaped';
 if(input.case==='data-root')component.runtimeReset.delete[0].path='.';
 if(input.case==='preserve-overlap')component.runtimeReset.delete.push({area:'data',path:'settings.json'});
 if(input.case==='duplicate-target')component.runtimeReset.delete.push({...component.runtimeReset.delete[0]});
 if(input.case==='completion-root')component.runtimeReset.completionMarker='.';
 if(input.case==='missing-preserve')component.runtimeReset.preserve[0].path='absent.json';
 if(input.case==='no-deployment')delete config.deployment;
 if(input.case==='duplicate-host')config.deployment.hosts.push({...config.deployment.hosts[0]});
 if(input.case==='remote-missing-cli')config.deployment.hosts.push({hostId:'remote',configPath:path.join(root,'remote-config.json')});
 if(input.case==='protected-software'){config.instances[0].paths={...paths,data:config.artifactRoot};await mkdir(config.artifactRoot,{recursive:true});component.runtimeReset.preserve=[];}
 if(input.case==='linked-delete'||input.case==='completion-link'){
  const outside=path.join(root,'outside');await mkdir(outside);await writeFile(path.join(outside,'kept.txt'),'outside fixture authority');
  await symlink(outside,path.join(paths.data,'linked'),process.platform==='win32'?'junction':'dir');
  if(input.case==='linked-delete')component.runtimeReset.delete[0].path='linked/runtime';else component.runtimeReset.completionMarker='linked/marker.json';
 }
 const names=['a','A','ä','a\u0308','😀','中'];
 if(input.case==='unicode-tree'){
  component.runtimeReset.preserve[0].path='settings';const directory=path.join(paths.data,'settings');await mkdir(directory);
  for(const name of names)await writeFile(path.join(directory,name),'preserved '+name);
 }
 await atomicJson(planPath,component);
 const journal=new HostJournal(config);journal.close();
 const comparisons=[];for(const a of names)for(const b of names)comparisons.push([a,b,Math.sign(a.localeCompare(b))]);
 const setup={config,configPath,distribution,comparisons,settings:path.join(paths.data,'settings.json'),journal:path.join(paths.data,'journal.sqlite'),sourceHash:sha(source),declarationHash:sha(declarations.join('\n')),before:{settings:sha(await readFile(path.join(paths.data,'settings.json'))),journal:sha(await readFile(path.join(paths.data,'journal.sqlite'))),plan:sha(await readFile(planPath))}};
 await writeFile(setupPath,JSON.stringify(setup));process.stdout.write(JSON.stringify(setup));
}else{
 const setup=JSON.parse(await readFile(setupPath,'utf8'));
 if(input.kind==='run'){let result;try{result={ok:true,value:await original(setup.config,setup.configPath,setup.distribution)};}catch(error){result={ok:false,error:IvyError.from(error).toWire()};}process.stdout.write(JSON.stringify(result));}
 else if(input.kind==='observe')process.stdout.write(JSON.stringify({settings:sha(await readFile(setup.settings)),journal:sha(await readFile(setup.journal)),plan:sha(await readFile(path.join(setup.distribution,'services/fixture/deploy.json'))),before:setup.before}));
 else assert.fail('Unknown reset planner operation');
}
