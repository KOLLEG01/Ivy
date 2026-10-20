// Execute the pinned web-push dependency with controlled ECDH, salt and clock.
import {createRequire,registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import crypto from 'node:crypto';
import {fileURLToPath,pathToFileURL} from 'node:url';
registerHooks({resolve(specifier,context,next){
 if(specifier==='web-push')return next(pathToFileURL(process.env.IVY_HIVE_TEST_WEB_PUSH).href,context);
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const require=createRequire(process.env.IVY_HIVE_TEST_WEB_PUSH);
const webPush=require(process.env.IVY_HIVE_TEST_WEB_PUSH);
const ece=require('http_ece');
const input=JSON.parse(readFileSync(0,'utf8'));
const curve=privateKey=>{const key=crypto.createECDH('prime256v1');key.setPrivateKey(Buffer.from(privateKey,'base64url'));return key;};
const scalar=n=>{const key=Buffer.alloc(32);key[31]=n;return key.toString('base64url');};
const pair=n=>{const key=curve(scalar(n));return{publicKey:key.getPublicKey().toString('base64url'),privateKey:scalar(n)};};
const receiver=pair(1),sender=pair(2),signer=pair(3);
function normalize(request, signingKey=signer){
 // HTTP serializes numeric header values; compare the emitted field values.
 const headers=Object.fromEntries(Object.entries(request.headers).map(([key,value])=>[key,String(value)])),auth=headers.Authorization;
 const token=auth.slice('vapid t='.length).split(', k=')[0],parts=token.split('.');
 const key=crypto.createPublicKey({key:{kty:'EC',crv:'P-256',x:curve(signingKey.privateKey).getPublicKey().subarray(1,33).toString('base64url'),y:curve(signingKey.privateKey).getPublicKey().subarray(33).toString('base64url')},format:'jwk'});
 const signatureValid=crypto.verify('sha256',Buffer.from(parts.slice(0,2).join('.')),{key,dsaEncoding:'ieee-p1363'},Buffer.from(parts[2],'base64url'));
 headers.Authorization=`vapid t=${parts.slice(0,2).join('.')}.<verified-signature>, k=${auth.split(', k=')[1]}`;
 return{endpoint:request.endpoint,headers,body:request.body?.toString('base64')??'',signatureValid};
}
if(input.op==='validate'){
 const {pushSubscription}=await import('../../services/hive/src/browser-push-store.ts');
 const generated=input.keys.map(key=>{try{return curve(key.privateKey).getPublicKey().toString('base64url')===key.publicKey&&Buffer.from(key.privateKey,'base64url').length===32;}catch{return false;}});
 const points=input.points.map(key=>{try{pushSubscription({endpoint:'https://fcm.googleapis.com/send/test',keys:{p256dh:key,auth:Buffer.alloc(16,7).toString('base64url')}});return true;}catch{return false;}});
 console.log(JSON.stringify({generated,points}));
}else{
 const create=crypto.createECDH,random=crypto.randomBytes,DateClass=globalThis.Date;
 const now=input.now??1700000000000;
 process.env.TZ=input.timezone??'UTC';
 let clockReads=0;
 globalThis.Date=class extends DateClass{constructor(...args){super(...(args.length?args:[now]));if(!args.length)++clockReads;}static now(){return now;}};
 const expiration=new Date();expiration.setSeconds(expiration.getSeconds()+12*60*60);
 const expectedExpiration=Math.floor(expiration.getTime()/1000);
 const salt=Buffer.from(Array.from({length:16},(_,i)=>i));
 crypto.createECDH=function(...args){const key=create(...args);key.generateKeys=function(){key.setPrivateKey(Buffer.from(sender.privateKey,'base64url'));return key.getPublicKey();};return key;};
 crypto.randomBytes=function(size,...rest){if(size===16)return Buffer.from(salt);return random(size,...rest);};
 const subscription={endpoint:'https://updates.push.services.mozilla.com/wpush/v2/test?q=%2F',keys:{p256dh:receiver.publicKey,auth:Buffer.alloc(16,7).toString('base64url')}};
 const requests=input.cases.map(spec=>{
  const sub={...subscription,...spec.subscription,keys:{...subscription.keys,...spec.subscription?.keys}};
  const details={...signer,subject:'mailto:push@example.test',...spec.details};
  clockReads=0;
  try{const request=webPush.generateRequestDetails(sub,spec.payload,{vapidDetails:details,TTL:3600,timeout:10000});return{ok:true,...normalize(request),clockReads};}catch{return{ok:false,clockReads};}
 });
 crypto.createECDH=create;crypto.randomBytes=random;globalThis.Date=DateClass;
 const native=(input.native??[]).map(item=>{
  const request={...item,body:Buffer.from(item.body,'base64')};
  try{const plain=request.body.length?ece.decrypt(request.body,{version:'aes128gcm',privateKey:curve(receiver.privateKey),authSecret:subscription.keys.auth}).toString('base64'):'';return{ok:true,...normalize(request),plaintext:plain};}catch{return{ok:false};}
 });
 console.log(JSON.stringify({receiver,sender,signer,subscription,expiration:expectedExpiration,requests,native}));
}
