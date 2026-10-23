import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createHash,randomUUID} from 'node:crypto';
import {cpSync,existsSync,mkdirSync,readFileSync,renameSync,writeFileSync} from 'node:fs';
import {createInterface} from 'node:readline';
import {dirname,isAbsolute,join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {parseArgs} from 'node:util';
import {setTimeout as delay} from 'node:timers/promises';
import {materializeNativePlan} from '../../../packages/host-runtime/src/native-plan.mjs';

// Installed Windows package only: loopback SIP/RPC, no INVITE, account,
// provider, physical device, Desktop, UIA, USB input or Codex process.
const {values}=parseArgs({options:{output:{type:'string'},artifact:{type:'string'}}});
assert.equal(process.platform,'win32');assert.equal(process.arch,'x64');
assert.ok(values.output&&isAbsolute(values.output));
const root=fileURLToPath(new URL('../../..',import.meta.url));
const source=resolve(values.artifact??root);
const plan=materializeNativePlan(JSON.parse(readFileSync(join(source,'services/phone-bridge/deploy.json'),'utf8')));
const work=join(dirname(values.output),'phone-package-'+randomUUID()),artifactRoot=join(work,'artifact');
mkdirSync(artifactRoot,{recursive:true});
cpSync(join(source,'dist'),join(artifactRoot,'dist'),{recursive:true,errorOnExist:true,force:false});
const build=JSON.parse(readFileSync(join(artifactRoot,'dist/build-info.json'),'utf8'));
const hash=path=>'sha256:'+createHash('sha256').update(readFileSync(path)).digest('hex');
const executable='dist/native/phone/Ivy.PhoneRuntime.exe';
const executableHash=hash(join(artifactRoot,executable));
const evsPath=join(artifactRoot,'dist/native/phone/ivy_phone_evs.dll'),evsHash=hash(evsPath);
const dataRoot=join(work,'data'),configPath=join(work,'config.json');
const report={phase:'not_proven',build,work,executableHash,evsHash,codecCount:0,toolCount:0,
  runtimePathEmpty:false,hashMismatchRejected:false,normalExit:null,restartExit:null,stoppedHealthRejected:false,
  realVoiceOrAudio:false,error:null};
const clean=Object.fromEntries(Object.entries(process.env).filter(([key])=>
  !/^(PATH|NODE_OPTIONS|IVY_LAUNCH_ID|IVY_INSTANCE_CONFIG|IVY_HIVE_HEALTH_URL)$/i.test(key)));
clean.PATH='';
async function bounded(work,label){
  let timer;try{return await Promise.race([work,new Promise((_,reject)=>{
    timer=setTimeout(()=>reject(Error(label+' did not settle')),120000);
  })]);}finally{clearTimeout(timer);}
}
const children=[];
function launch(file,args,env=clean){
  const child=spawn(file,args,{env,windowsHide:true,stdio:['pipe','pipe','pipe']});
  child.error='';child.stderr.on('data',bytes=>{child.error=(child.error+bytes).slice(-8192);});
  child.completed=new Promise((resolve,reject)=>{child.once('error',reject);child.once('close',resolve);});
  children.push(child);return child;
}
async function exited(child){return bounded(child.completed,'Child exit');}
function json(path,value){
  mkdirSync(dirname(path),{recursive:true});const temp=path+'.'+randomUUID()+'.tmp';
  writeFileSync(temp,JSON.stringify(value));renameSync(temp,path);
}
const health=()=>JSON.parse(readFileSync(join(dataRoot,'health.json'),'utf8'));
async function ready(child,priorBoot){
  return bounded((async()=>{for(;;){
    assert.equal(child.exitCode,null,child.error);
    if(existsSync(join(dataRoot,'health.json'))){const value=health();if(value.ready&&value.bootId!==priorBoot)return value;}
    await delay(25);
  }})(),'Service readiness');
}
async function stop(child,value){
  json(join(dataRoot,'control.json'),{schemaVersion:1,instanceId:value.instanceId,launchId:value.launchId,
    bootId:value.bootId,action:'shutdown',requestedAt:new Date().toISOString()});
  assert.equal(await exited(child),0,child.error);assert.equal(health().ready,false);
}
async function checkHealth(){
  const child=launch(join(artifactRoot,plan.readiness.command.executable),['--config',configPath]);
  let output='';child.stdout.on('data',bytes=>{output+=bytes;});
  const code=await exited(child);return {code,value:JSON.parse(output.trim())};
}
try{
  assert.equal(executableHash,hash(join(artifactRoot,'dist/native/win32-x64/ivy-phone-native.exe')));
  for(const name of ['coreclr.dll','Ivy.PhoneRuntime.dll','ivy-phone-input.dll'])
    assert.equal(existsSync(join(artifactRoot,'dist/native/phone',name)),false);
  assert.equal(existsSync(join(artifactRoot,'dist/native/ivy-phone-input.dll')),false);
  assert.equal(existsSync(join(artifactRoot,'dist/native/win32-x64/ivy-phone-input.dll')),false);
  assert.equal(hash(join(source,executable)),executableHash);
  const peer=launch(process.execPath,[join(root,'tests/fixtures/phone-service-peer.mjs')],process.env);
  const peerLines=createInterface({input:peer.stdout,crlfDelay:Infinity})[Symbol.asyncIterator]();
  const url=(await bounded(peerLines.next(),'Loopback peer')).value;
  assert.ok(url?.startsWith('http://127.0.0.1:'));
  // The real packaged C library is exercised through the real Rust native pipe.
  mkdirSync(join(work,'native-data'));
  const native=launch(join(artifactRoot,executable),['--epoch',randomUUID(),'--owner-root',join(work,'native-data')]);
  const lines=createInterface({input:native.stdout,crlfDelay:Infinity})[Symbol.asyncIterator]();
  let sequence=0;
  const request=async(method,params,effect=false)=>{
    const requestId=++sequence;
    native.stdin.write(JSON.stringify({version:1,epoch:nativeEpoch,requestId,
      operationId:effect?randomUUID():null,method,params})+'\n');
    for(;;){const line=await bounded(lines.next(),method);assert.equal(line.done,false,native.error);
      const value=JSON.parse(line.value);if(value.requestId!==requestId)continue;
      assert.equal(value.ok,true,JSON.stringify(value));return value.result;}
  };
  // Use the exact epoch passed to the child; no process-global discovery.
  const nativeEpoch=native.spawnargs[native.spawnargs.indexOf('--epoch')+1];
  await request('configure',{binding:{address:'127.0.0.1',port:0,transport:'udp'},
    codecs:{preferences:['EVS','PCMA'],packetMs:20},incomingPeers:[],registration:null,password:null},true);
  const codecs=await request('codec.test',{});assert.equal(codecs.codecs.length,5);
  for(const codec of codecs.codecs)assert.equal(codec.passed,true,JSON.stringify(codec));
  report.codecCount=codecs.codecs.length;
  const callId=randomUUID();await request('call.prepare',{callId},true);
  const prepared=await request('status',{});assert.equal(prepared.call.id,callId);
  assert.deepEqual(prepared.media.audio,{callId,state:'unprepared',route:null});
  await request('call.hangup',{callId},true);await request('call.release',{callId},true);
  native.stdin.end();assert.equal(await exited(native),0,native.error);
  const config={schemaVersion:1,hostId:'fixture',instanceId:'phone-package-fixture',serviceNodeId:'fixture.phone',
    componentId:'phone-bridge',publicBaseUrl:url,dataRoot,artifactRoot,workRoot:join(work,'work'),logsRoot:join(work,'logs'),
    buildId:build.buildId,version:build.version,credential:'isolated-fixture',settings:{
      native:{executable,executableHash},binding:{address:'127.0.0.1',port:0,transport:'udp'},
      codecs:{preferences:['PCMA'],packetMs:20},registration:null,credentialsPath:null,
      policy:{recipients:[],incoming:[]},ringSeconds:30,pollMs:250,incomingPrincipalId:null,
      codexVoice:null,incomingRoute:'windows',outgoingRoute:'windows',
      audio:{captureEndpointId:'fixture-capture',renderEndpointId:'fixture-render',sourceMode:'system_excluding_runtime',
        captureBufferMs:20,renderLatencyMs:20,queueMs:40}}};
  const bad=structuredClone(config);bad.settings.native.executableHash='sha256:'+'0'.repeat(64);json(configPath,bad);
  const failed=launch(join(artifactRoot,plan.entrypoint.executable),['--config',configPath],{...clean,IVY_LAUNCH_ID:randomUUID()});
  failed.stdout.resume();assert.equal(await exited(failed),1);assert.match(failed.error,/phone_build_mismatch/);
  report.hashMismatchRejected=true;json(configPath,config);
  const start=()=>{const child=launch(join(artifactRoot,plan.entrypoint.executable),['--config',configPath],
    {...clean,IVY_LAUNCH_ID:randomUUID()});child.stdout.resume();return child;};
  const service=start(),first=await ready(service);
  await bounded((async()=>{for(;;){
    const value=await (await fetch(new URL('fixture',url))).json();
    if(value.heartbeat?.ready){assert.equal(value.tools.length,23);report.toolCount=value.tools.length;return;}
    assert.equal(service.exitCode,null,service.error);await delay(25);
  }})(),'Tool registration');
  const healthy=await checkHealth();assert.equal(healthy.code,0,JSON.stringify(healthy));assert.equal(healthy.value.ok,true);
  report.runtimePathEmpty=true;
  json(join(dataRoot,'control.json'),{schemaVersion:1,instanceId:first.instanceId,launchId:first.launchId,
    bootId:randomUUID(),action:'shutdown',requestedAt:new Date().toISOString()});
  await delay(500);assert.equal(service.exitCode,null,service.error);
  await stop(service,first);report.normalExit=service.exitCode;
  const replacement=start(),second=await ready(replacement,first.bootId);
  await stop(replacement,second);report.restartExit=replacement.exitCode;
  const stopped=await checkHealth();assert.equal(stopped.code,1);assert.equal(stopped.value.code,'service_not_ready');
  report.stoppedHealthRejected=true;peer.stdin.end();assert.equal(await exited(peer),0,peer.error);
  assert.equal(hash(join(source,executable)),executableHash);assert.equal(hash(evsPath),evsHash);
  report.phase='passed';
}catch(error){report.error=error.code??error.message;throw error;}
finally{
  for(const child of children){
    if(child.exitCode===null){child.stdin.end();child.kill();await Promise.race([child.completed.catch(()=>{}),delay(10000)]);}
  }
  mkdirSync(dirname(values.output),{recursive:true});writeFileSync(values.output,JSON.stringify(report,null,2)+'\n',{flag:'wx'});
}
process.stdout.write('phone_rust_package_passed: native service/health/Job, 23 tools, five packaged codecs, hash refusal, stop/restart; PATH empty\n');
