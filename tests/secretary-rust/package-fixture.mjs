import assert from 'node:assert/strict';
import {copyFileSync,readFileSync,writeFileSync,mkdirSync,realpathSync,existsSync} from 'node:fs';
import {join,dirname} from 'node:path';import {createHash} from 'node:crypto';
import {buildNativeDispatchers} from '../../tools/build/build-native-dispatchers.mjs';
import {stageNativePayloads} from '../../tools/build/stage-native-payloads.mjs';
const [selected,service,health,job]=process.argv.slice(2),root=realpathSync(selected),dist=join(root,'dist');
const result=buildNativeDispatchers({component:'secretary',root:process.cwd(),outputRoot:dist});
const platform=process.platform+'-'+process.arch,suffix=process.platform==='win32'?'.exe':'';
const native=join(dist,'native',platform);mkdirSync(native,{recursive:true});
const programs=[['ivy-secretary',service],['ivy-health',health],...(process.platform==='win32'?[['ivy-job',job]]:[])];
const identity={schemaVersion:1,...JSON.parse(readFileSync(join(dist,'build-info.json'),'utf8'))};
for(const [name,source]of programs)copyFileSync(source,join(native,name+suffix));
if(process.platform==='win32')writeFileSync(join(dist,'native/ivy-job.exe'),'invalid legacy helper must never be selected');
writeFileSync(join(native,'build-info.json'),JSON.stringify(identity));
// This is a local launch fixture. It cannot be mistaken for a universal release:
// the unchanged real plan must refuse the three missing target payloads first.
const plan=join(root,'services/secretary/deploy.json');mkdirSync(dirname(plan),{recursive:true});copyFileSync('services/secretary/deploy.json',plan);
const target={'win32-x64':'x86_64-pc-windows-msvc','win32-arm64':'aarch64-pc-windows-msvc','linux-x64':'x86_64-unknown-linux-gnu','linux-arm64':'aarch64-unknown-linux-gnu'}[platform];
const record=(component,names)=>({component,target,platform,identity,outputDirectory:native,files:names.map(name=>join(native,name+suffix)),...(component==='secretary'?{instructionFiles:[join(dist,'instructions/secretary-notification-policy.json')]}:{})});
const blocked=join(root,'universal-must-refuse');
assert.throws(()=>stageNativePayloads({component:'secretary',root,outputRoot:blocked,identity,payloads:[record('secretary',['ivy-secretary']),record('host-executor',programs.slice(1).map(([name])=>name))],requiredBinaries:{win32:['ivy-job']}}),/Every advertised target requires/);
assert.equal(existsSync(blocked),false);
console.log(JSON.stringify({main:join(dist,'services/secretary/src/main.js'),health:join(dist,'packages/host-runtime/src/health.js'),job:process.platform==='win32'?join(native,'ivy-job.exe'):null,platform,missingMatrixRefused:true,files:result.files.map(path=>({path,sha256:createHash('sha256').update(readFileSync(path)).digest('hex')}))}));
