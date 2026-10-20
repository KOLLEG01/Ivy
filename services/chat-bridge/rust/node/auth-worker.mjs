// One generation/account, no socket or ChatBridge TypeScript business backend.
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {authFrames} from './auth-frames.mjs';

const diagnostics = {log:0,trace:0,debug:0,info:0,warn:0,error:0,other:0};
for (const key of ['log','trace','debug','info','warn','error','dir','dirxml','table','assert']) {
  console[key] = () => { diagnostics[Object.hasOwn(diagnostics,key)?key:'other']++; };
}
let adapter, active, pending, callbackNumber=0, lastRequest=0n, stopped=false;
const generation=process.argv[2];
const frames=authFrames(process.stdin,process.stdout,generation);
function fence() {
  if (stopped) return;
  stopped=true; adapter?.fence();
  // Rejection into commitWithRetry would leave a live crypto task. Terminate
  // this disposable child; the Rust owner retains Unknown and never replays.
  process.exit(2);
}
process.on('uncaughtException',fence);
process.on('unhandledRejection',fence);
const safeError = error => ['TypeError','SessionError','MessageCounterError','PreKeyError'].includes(error?.name) ? error.name : 'Error';
async function callback(kind,entries) {
  if (stopped || !active || pending) throw Error('Invalid authentication callback.');
  const callback=String(++callbackNumber);
  const answer=new Promise((resolve,reject) => { pending={callback,kind:kind==='get'?'rows':'committed',resolve,reject}; });
  await frames.send(active.request,{kind,callback,entries});
  return answer;
}
// Serialize even the library's Promise.all reads/writes to bound callbacks.
let callbacks=Promise.resolve();
function raw(kind,entries) {
  const work=callbacks.then(() => callback(kind,entries)); callbacks=work; return work;
}
try {
  if (!/^\d+$/.test(generation??'')) fence();
  const local=createRequire(import.meta.resolve('baileys'));
  const baileys=JSON.parse(readFileSync(new URL('../package.json',import.meta.resolve('baileys'))));
  const libsignal=JSON.parse(readFileSync(local.resolve('libsignal/package.json')));
  if (baileys.version!=='7.0.0-rc13' || libsignal.version!=='6.0.0') fence();
  const {createAuthAdapter}=await import('./auth-adapter.mjs');
  for await (const {request,value} of frames.messages()) {
    if (active && pending && request===active.request && value.callback===pending.callback && value.kind===pending.kind) {
      const completion=pending; pending=null;
      completion.resolve(value.kind==='rows'?value.rows:undefined);
      continue;
    }
    if (active || !/^\d+$/.test(request) || BigInt(request)!==lastRequest+1n || typeof value.operationId!=='string') fence();
    lastRequest++; callbackNumber=0; callbacks=Promise.resolve(); active={request,operationId:value.operationId};
    const command=active;
    void (async () => {
      try {
        let result=null;
        if (value.kind==='init' && !adapter) {
          adapter=await createAuthAdapter(value.creds,{get:entries=>raw('get',entries),set:entries=>raw('set',entries)});
          result=JSON.stringify({baileys:baileys.version,libsignal:libsignal.version});
        } else if (value.kind==='call' && adapter) result=await adapter.call(value.method,value.arguments);
        else if (value.kind==='stop' && adapter) {
          adapter.close();
          await frames.send(request,{kind:'stopped',operationId:command.operationId,diagnostics});
          stopped=true; process.stdin.destroy(); return;
        } else fence();
        await frames.send(request,{kind:'result',operationId:command.operationId,result});
      } catch(error) {
        if (error?.name==='AuthFenced' || pending) fence();
        await frames.send(request,{kind:'error',operationId:command.operationId,name:safeError(error)});
      } finally { if (!stopped) active=null; }
    })().catch(fence);
  }
  if (!stopped) fence();
} catch { fence(); }
