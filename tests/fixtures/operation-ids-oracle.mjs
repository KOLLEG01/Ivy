import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.endsWith('.js')&&specifier.startsWith('.')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {runtimeEpoch,newOperationId,scopedOperationId,parseOperationId}=await import('../../packages/sdk/src/client.ts');
let now=1000,calls=0;
Date.now=()=>now;
const client={async request(){calls++;return {runtimeEpoch:'epoch'};}};
const epochs=await Promise.all([runtimeEpoch(client),runtimeEpoch(client)]);
const manual=await newOperationId(client,'\ud800');
const first=await scopedOperationId(client,{b:2,a:1});
const same=await scopedOperationId(client,{a:1,b:2});
now+=86400000-1;
const before=await scopedOperationId(client,{a:1,b:2});
now++;
const after=await scopedOperationId(client,{a:1,b:2});
now=100000000;
const later=await scopedOperationId(client,'later');
now=0;
const earlier=await scopedOperationId(client,'earlier');
now=86400001;
const retained=await scopedOperationId(client,'earlier');
let retryCalls=0;
const retry={async request(){if(++retryCalls===1)throw Error('offline');return {runtimeEpoch:'second'};}};
const failed=(await Promise.allSettled([scopedOperationId(retry,'scope'),scopedOperationId(retry,'scope')])).map(r=>r.status);
const recovered=await scopedOperationId(retry,'scope');
let invalidCalls=0,invalidCode;
try{await scopedOperationId({async request(){invalidCalls++;}},'x'.repeat(65537));}catch(error){invalidCode=error.code;}
process.stdout.write(JSON.stringify({epochs,manual,same:first===same,before:first===before,expired:first!==after,issuedAt:parseOperationId(after).issuedAtUnixMs,retainedBehindLater:earlier===retained,calls,retryCalls,failed,recoveredEpoch:parseOperationId(recovered).runtimeEpoch,invalidCalls,invalidCode}));
