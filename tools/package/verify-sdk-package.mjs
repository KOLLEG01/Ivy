import { mkdtemp, mkdir, writeFile, rm, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, relative } from 'node:path';
import { spawn } from 'node:child_process';
import { HiveServer } from '../../dist/services/hive/src/server.js';
import { digest } from '../../dist/packages/contracts/src/canonical.js';
const root = await mkdtemp(join(tmpdir(), 'ivy-sdk-package-'));
const packageReceipt = JSON.parse(await readFile(resolve('.local/packages/sdk-package.json'), 'utf8'));
const packagePath = resolve(packageReceipt.path);
let server;
function command(args, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, args, { cwd, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = ''; const timer = setTimeout(() => child.kill(), 60_000);
    child.stdout.on('data', bytes => { output = (output + bytes.toString()).slice(-65536); });
    child.stderr.on('data', bytes => { output = (output + bytes.toString()).slice(-65536); });
    child.once('error', reject);
    child.once('close', code => { clearTimeout(timer); if (code !== 0) reject(new Error('Isolated package command failed: ' + output)); else resolve(output); });
  });
}
try {
  await writeFile(join(root, 'package.json'), JSON.stringify({ name: 'isolated-ivy-sdk-consumer', version: '1.0.0', packageManager: 'npm@11.16.0', private: true, type: 'module', dependencies: { '@ivy/sdk': 'file:' + packagePath.replaceAll('\\', '/') } }));
  await command([process.env.npm_execpath, 'install', '--ignore-scripts', '--no-audit', '--no-fund'], root);
  await mkdir(join(root, 'services/companion/extension'), { recursive: true });
  await writeFile(join(root, 'services/companion/extension/service-worker.mjs'), 'export const version = 1;\n');
  await writeFile(join(root, '.gitignore'), 'node_modules/\n');
  await writeFile(join(root, 'verify-source.mjs'), `import assert from 'node:assert/strict';
    import {checkedNativeContract, nativeVersions} from '@ivy/sdk/node';
    for (const version of nativeVersions) {
      const retained = checkedNativeContract(version);
      assert.equal(retained.catalog.version, version);
      assert.equal(checkedNativeContract(version, {catalogHash:retained.catalogHash}).catalogHash, retained.catalogHash);
    }
    import {spawnSync} from 'node:child_process'; import {writeFileSync} from 'node:fs';
    import {buildIdentity} from '@ivy/sdk/host';
    const git=spawnSync('git',['init','--quiet'],{windowsHide:true,encoding:'utf8'}); assert.equal(git.status,0,git.stderr);
    const before=buildIdentity(); writeFileSync('services/companion/extension/service-worker.mjs','export const version = 2;\\n');
    const after=buildIdentity(); assert.deepEqual(Object.keys(before).sort(),['buildId','schemaVersion','version']); assert.notEqual(before.buildId,after.buildId);
    console.log('standalone_build_identity_verified');`);
  const sourceOutput = await command(['verify-source.mjs'], root);
  if (!sourceOutput.includes('standalone_build_identity_verified')) throw new Error('The packaged SDK did not expose opaque standalone build identity.');
  await writeFile(join(root, 'consumer-types.ts'), `import type {Agent, TaskBoard, Automation, Chat, Wire} from '@ivy/sdk';
    import {CodexAppTools} from '@ivy/sdk/codex-app-tools';
    import type {AppToolsSettings} from '@ivy/sdk/codex-app-tools';
    function appTools(settings: AppToolsSettings) { return new CodexAppTools(settings); } void appTools;
    import {validateAgent, NativeOwner, reconcileNativeOperation, nativeServiceTools, serviceTools} from '@ivy/sdk/node';
    import type {RpcClient, NativeTarget, NativeOperationCall} from '@ivy/sdk/node';
    import {buildIdentity, HealthFile, checkHealth, instanceConfig} from '@ivy/sdk/host';
    import type {Host} from '@ivy/sdk/host';
    const build: string = buildIdentity().buildId; const health: typeof HealthFile = HealthFile;
    const instance: Promise<Host.InstanceConfig> = instanceConfig('C:/unexecuted-type-check/config.json'); void [build,health,instance,checkHealth];
    const absent: Agent.OperationAbsence = {kind:'agent_operation_absent',operationId:'original',serviceNodeId:'owner',epoch:null};
    validateAgent('OperationAbsence', absent);
    const read: Agent.ReadInput = {nativeVersion:'0.154.0',method:'thread/read',params:{threadId:'original',includeTurns:false}}; validateAgent('ReadInput',read);
    const identity: Agent.InputIdentity = {serviceNodeId:absent.serviceNodeId,epoch:'epoch',requestId:1};
    const task: TaskBoard.TaskFields = {title:'Public SDK consumer',description:'',acceptanceCriteria:[],category:null,control:'user',priority:1,executionRequirement:null,workspaceRequirement:{kind:'task_workspace'},dependencies:[],userContact:'ticket',nextReviewAt:null,dueAt:null};
    const pending: Automation.Pending = {periodDate:'2026-09-07',advanceToDate:'2026-09-08',skippedFromDate:null,skippedThroughDate:null,snapshot:null};
    const value: Wire.Json = {identity,task,pending}; void value;
    function originalOperation(client: RpcClient, target: NativeTarget, call: NativeOperationCall) {
      const owner = new NativeOwner(client, 'consumer', target, {signal:new AbortController().signal});
      void nativeServiceTools(client,target.serviceNodeId).binding('codex.'+call.method,call.definitionHash);
      return reconcileNativeOperation({owner,call,journal:{current:0,previous:null,wasObserved:false,retain:async()=>1}});
    }
    void originalOperation;`);
  await command([resolve('node_modules/typescript/bin/tsc'), '--module', 'NodeNext', '--target', 'ES2023', '--strict', '--skipLibCheck', '--noEmit', 'consumer-types.ts'], root);
  server = new HiveServer({ filename: join(root, 'hive.sqlite'), publicBaseUrl: 'http://127.0.0.1/ivy', version: 'package-test', buildId: digest('package-test'), credentials: [{ principalId: 'package-test', digest: digest('isolated-package-token') }], listenPort: 0 });
  const { port } = await server.start();
  await writeFile(join(root, 'native-catalog.json'), await readFile('specs/native/codex-0.154.0/catalog.json'));
  await writeFile(join(root, 'verify.mjs'), `import {HiveClient,ServiceClient,discover,callBound,serviceTools,NativeOwner,reconcileNativeOperation,digest,validateAgent,NativeContract,nativeTurnItems,saveNativeParameters,readNativeParameters} from '@ivy/sdk/node';
    import {CodexAppTools} from '@ivy/sdk/codex-app-tools';
    import assert from 'node:assert/strict'; import {readFileSync} from 'node:fs';
    assert.equal(typeof CodexAppTools.inspect,'function');
    assert.throws(()=>new CodexAppTools({}));
    validateAgent('OperationAbsence',{kind:'agent_operation_absent',operationId:'original',serviceNodeId:'owner',epoch:null});
    let rejected=false;try{validateAgent('OperationAbsence',{kind:'agent_operation_absent',operationId:'original',serviceNodeId:'owner'});}catch{rejected=true;}if(!rejected)throw new Error('Malformed owner evidence was accepted');
    const base='http://127.0.0.1:${port}/ivy';
    const client=new HiveClient(base,{credential:'isolated-package-token'});
    const contract=new NativeContract(JSON.parse(readFileSync('native-catalog.json','utf8'))), item={id:'original',type:'agentMessage',text:'Packaged schema interpretation.'};
    assert.deepEqual(nativeTurnItems(contract,{data:[{turnId:'turn',item}],nextCursor:null},'turn'),[item]);
    assert.throws(()=>nativeTurnItems(contract,{data:[{turnId:'foreign',item}],nextCursor:null},'turn'));
    const pin=await saveNativeParameters(client,contract,{method:'account/rateLimits/read',params:null,parentId:null,mutationId:'package-original-parameters'});
    assert.equal(await readNativeParameters(client,contract,{method:'account/rateLimits/read',parentId:null,ref:pin}),null);
    await assert.rejects(saveNativeParameters(client,contract,{method:'account/rateLimits/read',params:{unrelated:true},parentId:null,mutationId:'package-invalid-parameters'}));
    const service=new ServiceClient({publicBaseUrl:base,credential:()=> 'isolated-package-token',identity:{serviceNodeId:'package-node',hostId:'isolated',serviceName:'package-test',version:'test',buildId:digest('package'),hiveProtocol:1},
      registry:()=>({namespaces:[{namespace:'package-test',description:'Isolated installed package',guideMarkdown:'',topics:[],inventoryKinds:[],tools:[{namespace:'package-test',name:'read',description:'Concrete installed package handler',interfaceVersion:'1.0.0',inputSchema:{type:'object',additionalProperties:false},outputSchema:{type:'string'}}]}],contracts:[],requiredContracts:[]}),
      handlers:{'package-test.read':()=> 'standalone package works'},reconcile:async connection=>{await connection.request('system.status',{})}});
    service.start();try{await service.waitReady();const result=await callBound(client,await discover(client,'package-test.read'),{});if(result!=='standalone package works')throw new Error('Unexpected result');
      const bound=serviceTools(client,'package-node',[{namespace:'package-test',interfaceVersion:'1.0.0'}]);const first=await bound.binding('package-test.read');assert.equal(await bound.binding('package-test.read'),first);assert.equal(Object.isFrozen(first.definition),true);assert.equal(await bound.call('package-test.read',{}),result);
      assert.equal(typeof NativeOwner,'function');assert.equal(typeof reconcileNativeOperation,'function');
      console.log(JSON.stringify({ok:true,code:'isolated_sdk_verified',result}));}finally{await service.stop();}`);
  const output = await command(['verify.mjs'], root);
  if (!output.includes('isolated_sdk_verified')) throw new Error('No isolated SDK completion evidence.');
  console.log(JSON.stringify({ ok: true, code: 'isolated_sdk_verified', packagePath }));
} finally {
  await server?.close();
  if (!relative(tmpdir(), root).startsWith('ivy-sdk-package-')) throw new Error('Unsafe test cleanup');
  await rm(root, { recursive: true, force: true });
}
