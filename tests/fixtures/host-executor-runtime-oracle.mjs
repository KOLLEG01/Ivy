import {readFileSync,existsSync} from 'node:fs';
import {statfs} from 'node:fs/promises';
import {createRequire,registerHooks} from 'node:module';
import {fileURLToPath,pathToFileURL} from 'node:url';
const input=JSON.parse(readFileSync(0,'utf8'));
const require=createRequire(process.env.IVY_TEST_ORIGINAL_DEPENDENCIES+'/package.json');
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}
 try{return next(specifier,context);}catch(error){if(/^[a-z@]/.test(specifier)&&!specifier.startsWith('node:')&&!specifier.startsWith('data:'))return next(pathToFileURL(require.resolve(specifier)).href,context);throw error;}
}});
const ts=require(process.env.IVY_TEST_TYPESCRIPT_MODULE),source=readFileSync(new URL('../../packages/host-runtime/src/executor.ts',import.meta.url),'utf8'),ast=ts.createSourceFile('executor.ts',source,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);
const {HostJournal}=await import('../../packages/host-runtime/src/journal.ts');
const {materializeTarget}=await import('../../packages/host-runtime/src/target.ts');
const {verifyLaunchCandidate}=await import('../../packages/host-runtime/src/artifact.ts');
const {verifyServiceStorage}=await import('../../packages/host-runtime/src/service-storage.ts');
const {requireRuntimeRequirements}=await import('../../packages/host-runtime/src/package-requirements.ts');
const {verifyConfiguredNativeCatalog,verifyNativeCatalogCompatibility}=await import('../../packages/host-runtime/src/native-catalog-compatibility.ts');
const {agentManagerAccountSettings}=await import('../../packages/host-runtime/src/layout.ts');
const {resolveCommand,runCommand}=await import('../../packages/host-runtime/src/process.ts');
const {jsonFile}=await import('../../packages/host-runtime/src/config.ts');
const {HiveClient}=await import('../../packages/sdk/src/client.ts');
const {localHiveEndpoint}=await import('../../packages/host-runtime/src/health.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
if(input.mode==='offline-inspection'){
 const {join}=await import('node:path');
 const {instanceConfig,configurationPath}=await import('../../packages/host-runtime/src/config.ts');
 const {inspectStoppedStorage}=await import('../../services/hive/src/inspect-storage.ts');
 const text=readFileSync(new URL('../../services/hive/src/inspect-storage.ts',import.meta.url),'utf8'),tree=ts.createSourceFile('inspect-storage.ts',text,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);
 const body=tree.statements.find(n=>ts.isIfStatement(n)&&n.expression.getText(tree).startsWith('process.argv[1]')).thenStatement.getText(tree);
 process.argv=[process.execPath,'original-inspect-storage.js','--contracts',JSON.stringify(input.contracts),'--config',input.configurationPath];
 globalThis.__ivyOriginalOffline={join,instanceConfig,configurationPath,inspectStoppedStorage,validateHost,IvyError,requireThat};
 const code=ts.transpileModule('const {'+Object.keys(globalThis.__ivyOriginalOffline).join(',')+'}=globalThis.__ivyOriginalOffline;\n'+body,{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText;
 await import('data:text/javascript;base64,'+Buffer.from(code).toString('base64'));
}else{
const declaration=ast.statements.find(n=>ts.isClassDeclaration(n)&&n.name?.text==='HostExecutor');
const members=declaration.members.filter(n=>n.name&&['serviceStorage','compatibility'].includes(n.name.getText(ast))).map(n=>n.getText(ast));
const functions=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&['stoppedTargetIsConclusive','runningHiveCanBeInspected'].includes(n.name?.text)||ts.isVariableStatement(n)&&n.declarationList.declarations.some(d=>d.name.getText(ast)==='fresh')).map(n=>n.getText(ast));
globalThis.__ivyNativeCompatibility={HostJournal,materializeTarget,verifyLaunchCandidate,verifyServiceStorage,requireRuntimeRequirements,verifyConfiguredNativeCatalog,verifyNativeCatalogCompatibility,agentManagerAccountSettings,resolveCommand,runCommand,jsonFile,HiveClient,localHiveEndpoint,validateHost,IvyError,requireThat,statfs};
const code=ts.transpileModule('const {'+Object.keys(globalThis.__ivyNativeCompatibility).join(',')+'}=globalThis.__ivyNativeCompatibility;\n'+functions.join('\n')+'\nexport class Original {journal;bootstrapRoot;config;'+members.join('\n')+'}',{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText;
const {Original}=await import('data:text/javascript;base64,'+Buffer.from(code).toString('base64'));
const journal=new HostJournal(input.config),original=new Original();Object.assign(original,{journal,config:input.config,bootstrapRoot:input.bootstrapRoot});
try{await original.compatibility(input.candidateId,'fixture',input.settings);process.stdout.write(JSON.stringify({ok:true}));}
catch(error){process.stdout.write(JSON.stringify({ok:false,error:IvyError.from(error).toWire()}));}
finally{journal.close();}
}
