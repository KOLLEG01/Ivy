// Run the original admission class; only the SDK barrel import is narrowed.
import {readFileSync, existsSync} from 'node:fs';
import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
const contracts = new URL('../../../packages/contracts/src/', import.meta.url);
const sdk = 'data:text/javascript;base64,' + Buffer.from([
  `export {canonical,hashJson} from '${new URL('canonical.ts',contracts)}';`,
  `export {requireThat} from '${new URL('errors.ts',contracts)}';`,
  `export {validateChat} from '${new URL('chat-validation.ts',contracts)}';`,
].join('\n')).toString('base64');
registerHooks({resolve(specifier,context,next){
  if(specifier==='../../../packages/sdk/src/node.js')return {url:sdk,shortCircuit:true};
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
},load(url,context,next){
  if(url.endsWith('.ts'))return {format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
}});
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const channel={adapter:'whatsapp',accountId:'account',channelId:'channel'};
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,
  project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},
  nativePlan:unpersistedNativePlan(),channels:[{channel,displayName:'Original channel'}]};
const admission=new ChatAdmission(definition);
const request={action:'send',operationId:'original-operation',expectedBridge:admission.expected('alice'),channel,
  messageId:'message-one',expectedBinding:{objectId:'binding-one',revision:1},payload:{text:'Original input',images:[]}};
const image={object:{objectId:'image-one',revision:1},contentHash:'sha256:'+'a'.repeat(64),byteLength:1,mediaType:'image/png',label:'original image'};
const rows=[];
function add(kind,def=definition,req=request,caller='alice'){
  const row={kind,definition:def,request:req,caller};
  try {
    const a=new ChatAdmission(def);
    row.result={value:kind==='send'?a.send(caller,req):kind==='authorize'?(a.authorize(caller,req.expectedBridge,req.channel),null):
      {definitionHash:a.definitionHash,expected:a.expected(caller),channels:a.channelsFor(caller)}};
  } catch(cause){const e=IvyError.from(cause);row.result={error:{code:e.code,message:e.message,outcome:e.outcome}};}
  rows.push(row);
}
add('definition');add('send');
for(const caller of ['alice','Alice','bob','\ud800','😀'])add('send',definition,request,caller);
for(const field of ['principalId','callerPrincipalId','workspaceId','definitionHash'])for(const value of ['different','sha256:'+'b'.repeat(64)])
  add('send',definition,{...request,expectedBridge:{...request.expectedBridge,[field]:value}});
for(const field of ['adapter','accountId','channelId'])for(const value of ['different','Account','\ud800','😀'])
  add('send',definition,{...request,channel:{...channel,[field]:value},payload:null}); // admission must win before payload validation
for(const text of ['', ' ', '\ud800','😀'.repeat(65536),'😀'.repeat(65537)])add('send',definition,{...request,payload:{text,images:[]}});
for(const images of [[image],[{...image,byteLength:0}],[{...image,byteLength:2097152},{...image,byteLength:2097152}],
  [{...image,byteLength:2097152},{...image,byteLength:2097152},image]])add('send',definition,{...request,payload:{text:'',images}});
for(const value of [null,{objectId:'binding-one',revision:2}])add('send',definition,{...request,expectedBinding:value});
add('send',definition,{...request,operationId:'new-transport-operation'});
for(const key of ['messageId','operationId','payload','expectedBinding']){const req=structuredClone(request);delete req[key];add('send',definition,req);}
for(const key of Object.keys(definition)){const def=structuredClone(definition);delete def[key];add('definition',def);}
for(const key of ['namespace','kind'])add('definition',{...definition,project:{...definition.project,[key]:'other'}});
add('definition',{...definition,channels:[...definition.channels,...definition.channels]});
add('definition',{...definition,nativePlan:{...definition.nativePlan,threadStart:{...definition.nativePlan.threadStart,params:{opaque:'x'.repeat(131072)}}}});
for(const text of ['Changed name','\ud800','😀'])add('definition',{...definition,channels:[{channel,displayName:text}]});
for(const expectedBridge of [null,{},request.expectedBridge])add('authorize',definition,{expectedBridge});
// Independent copies cannot alter the accepted definition or its hash.
const mutable=structuredClone(definition),copy=new ChatAdmission(mutable),before=copy.definitionHash;
mutable.channels[0].displayName='changed';copy.channelsFor('alice')[0].displayName='changed';
if(copy.definitionHash!==before||copy.channelsFor('alice')[0].displayName!=='Original channel')throw Error('Original copy contract changed');
process.stdout.write(JSON.stringify(rows));
