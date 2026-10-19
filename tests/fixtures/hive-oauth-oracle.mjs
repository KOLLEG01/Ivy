// Run the original OAuth class and unchanged worker handle over real SQLite.
// URLSearchParams mutation is observed as an owner boundary; redirect byte
// serialization remains with that owner and is not a native parity claim here.
import {registerHooks,stripTypeScriptTypes,syncBuiltinESMExports} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import crypto from 'node:crypto';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {digest,canonical}=await import('../../packages/contracts/src/canonical.ts');
const {HiveStore}=await import('../../services/hive/src/store.ts');
const input=JSON.parse(readFileSync(0,'utf8')),events=[],results={},values={},gates=new Map(),running=new Map(),snapshots=[];
let now=100000,counter=0,credentials=input.credentials??[{principalId:'alpha',digest:digest('credential')}];
const store=new HiveStore(':memory:');store.setMetadata('cursor_secret','33'.repeat(32));store.configureCredentials(credentials);
const random=crypto.randomBytes;
crypto.randomBytes=length=>{
 const number=counter++,bytes=Buffer.from(Array.from({length},(_,i)=>(number*17+i)%256));bytes.writeBigUInt64LE(BigInt(number));
 events.push({kind:'random',length,hex:bytes.toString('hex')});return bytes;
};syncBuiltinESMExports();
const {HiveOAuth}=await import('../../services/hive/src/oauth.ts');
const worker=readFileSync(new URL('../../services/hive/src/worker.ts',import.meta.url),'utf8');
const start=worker.indexOf('const oauthPrefix = '),end=worker.indexOf('let chain = Promise.resolve();');
if(start<0||end<start)throw Error('Original worker identity span unavailable');
const create=new Function('kernel','canonical','digest','createHmac','randomBytes','IvyError','requireThat','browserPush',stripTypeScriptTypes(worker.slice(start,end),{mode:'strip'})+';return handle;');
const handle=create({store},canonical,digest,crypto.createHmac,crypto.randomBytes,IvyError,requireThat,undefined);
const specs=[...(input.familyReplies??[])];
async function request(payload){
 events.push({kind:'worker',encoded:JSON.stringify(payload)});
 const spec=specs.shift()??{};
 if(spec.gate)await new Promise(resolve=>gates.set(spec.gate,resolve));
 if(spec.error)throw new IvyError(spec.error,'Isolated family failure.');
 return handle(payload);
}
const families={create:(state,activeCredentialReferences)=>request({action:'oauth.family.create',state,activeCredentialReferences}),rotate:rotation=>request({action:'oauth.family.rotate',rotation})};
let owner;
const originalNow=Date.now;Date.now=()=>now;
async function construct(){owner=new HiveOAuth(new URL(input.base??'https://example.test/ivy/'),credentials,families);if(input.initialize!==false)owner.initialize(await handle({action:'oauth.initialize'}));}
const resolve=value=>{
 if(value&&typeof value==='object'&&value.$ref){const [id,...path]=value.$ref.split('.');return path.reduce((v,k)=>v[k],values[id]);}
 if(value&&typeof value==='object'&&value.$units)return String.fromCharCode(...value.$units);
 if(Array.isArray(value))return value.map(resolve);
 if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,resolve(v)]));
 return value;
};
const record=(id,value,error)=>{if(error)results[id]={error:IvyError.from(error).toWire()};else{values[id]=value;results[id]={encoded:JSON.stringify(value)};}};
const flush=()=>new Promise(resolve=>setImmediate(resolve));
const Url=globalThis.URL,set=URLSearchParams.prototype.set;
let redirecting=false;
globalThis.URL=class extends Url {constructor(...args){super(...args);if(redirecting)events.push({kind:'redirect',uri:String(args[0])});}};
URLSearchParams.prototype.set=function(k,v){if(redirecting)events.push({kind:'redirect.set',key:String(k),value:String(v)});return set.call(this,k,v);};
try{
 await construct();
 for(const raw of input.steps){const step=resolve(raw),id=step.id;
  if(step.op==='clock')now=step.now;
  else if(step.op==='snapshot')snapshots.push({name:step.name,events:events.splice(0),results:structuredClone(results),families:store.all("SELECT key,value FROM metadata WHERE key LIKE 'oauth_refresh:%' ORDER BY key")});
  else if(step.op==='release'){const release=gates.get(step.gate);if(!release)throw Error('Missing gate');gates.delete(step.gate);release();}
  else if(step.op==='join')for(const key of step.ids)await running.get(key);
  else if(step.op==='sql')store.db.exec(step.sql);
  else if(step.op==='restart')await construct();
  else if(step.op==='setCredentials'){credentials=step.credentials;owner.setCredentials(credentials);}
  else if(['token','spawnToken','abandon'].includes(step.op)){
   const work=owner.token(new URLSearchParams(step.form));
   if(step.op==='abandon')void work.catch(()=>{});
   else {const observed=work.then(value=>record(id,value),error=>record(id,undefined,error));if(step.op==='token')await observed;else running.set(id,observed);}
  }else {
   try{
    let value;
    if(step.op==='register')value=owner.register(step.body);
    else if(step.op==='begin'){const url=new URL('https://example.test/oauth/authorize');url.search=new URLSearchParams(step.form).toString();value=owner.begin(url);}
    else if(step.op==='transaction')value=owner.transaction(step.transaction);
    else if(step.op==='authorize'){
     let uri;redirecting=true;try{uri=owner.authorize(step.transaction,step.credentialDigest);}finally{redirecting=false;}
     value=new URL(uri).searchParams.get('code');
    }else if(step.op==='auth')value=owner.authenticate(step.token,step.resource);
    else if(step.op==='metadata')value=owner.protectedResourceMetadata(step.resource);
    else if(step.op==='serverMetadata')value=owner.authorizationServerMetadata(step.basePath??'/ivy');
    else if(step.op==='initialize'){owner.initialize(step.secrets);value=null;}
    else if(step.op==='payload')value=JSON.parse(Buffer.from(step.token.split('.')[1],'base64url').toString('utf8'));
    else if(step.op==='mutate'){
     const parts=step.token.split('.');
     if(step.mode==='signature-padding')parts[2]+='=ignored';
     else if(step.mode==='signature-noise')parts[2]=parts[2].split('').join(' !\t');
     else if(step.mode==='signature-low-bytes')parts[2]=Array.from(parts[2],c=>String.fromCharCode(c.charCodeAt(0)+256)).join('');
     else if(step.mode==='signature-unused-bits'){const alphabet='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';const last=alphabet.indexOf(parts[2].at(-1));parts[2]=parts[2].slice(0,-1)+alphabet[last^1];}
     else if(step.mode==='payload-low-bytes'){
      parts[1]=Array.from(parts[1],c=>String.fromCharCode(c.charCodeAt(0)+256)).join('');
      const secrets=await handle({action:'oauth.initialize'});parts[2]=crypto.createHmac('sha256',Buffer.from(secrets.signingKey,'hex')).update(parts[1]).digest('base64url');
     }else throw Error('Unknown token mutation');
     value=parts.join('.');
    }
    else if(step.op==='forge'){
     const secrets=await handle({action:'oauth.initialize'}),payload=structuredClone(step.payload??null);
     for(const [key,value] of Object.entries(step.patch??{}))payload[key]=value;
     for(const key of step.omit??[])delete payload[key];
     const encoded=step.encoded??Buffer.from(JSON.stringify(payload)).toString('base64url');
     const signature=crypto.createHmac('sha256',Buffer.from(secrets.signingKey,'hex')).update(encoded).digest('base64url');
     value='ivyoa2.'+encoded+'.'+signature;
    }else throw Error('Unknown fixture command '+step.op);
    record(id,value);
   }catch(error){record(id,undefined,error);}
  }
  if(!step.noFlush)await flush();
 }
 await Promise.all(running.values());await flush();
 snapshots.push({name:'final',events,results,families:store.all("SELECT key,value FROM metadata WHERE key LIKE 'oauth_refresh:%' ORDER BY key")});
 process.stdout.write(JSON.stringify({snapshots}));
}finally{globalThis.URL=Url;URLSearchParams.prototype.set=set;Date.now=originalNow;crypto.randomBytes=random;syncBuiltinESMExports();store.close();}
