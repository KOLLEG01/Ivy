// Run the original admission class; only the SDK barrel import is narrowed.
import {readFileSync, existsSync} from 'node:fs';
import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
const contracts = new URL('../../../packages/contracts/src/', import.meta.url);
const sdk = 'data:text/javascript;base64,' + Buffer.from([
  `export {canonical,hashJson,digest} from '${new URL('canonical.ts',contracts)}';`,
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
if(process.argv[2]==='media'){
  const {admittedNativeInput}=await import('../../../services/chat-bridge/src/input-media.ts');
  const {digest}=await import('../../../packages/contracts/src/canonical.ts');
  const cases=[];
  function media(bytes,type){
    const item={...image,mediaType:type,byteLength:bytes.length,contentHash:digest(bytes)};
    return {request:{...request,payload:{text:'caption',images:[item]}},response:{object:{id:'image-one',contractKey:'chat-bridge/'+type.replace('/','-')},
      revision:{revision:1,contractVersion:'1.0.0',mediaType:type,byteLength:bytes.length,contentHash:item.contentHash},content:{encoding:'base64',value:bytes.toString('base64')}}};
  }
  const png=Buffer.from([137,80,78,71,13,10,26,10]);
  const webp=(chunk='VP8 ',size=0,padding=0)=>{const b=Buffer.alloc(20+size+padding);b.write('RIFF');b.writeUInt32LE(b.length-8,4);b.write('WEBP',8);b.write(chunk,12);b.writeUInt32LE(size,16);return b;};
  for(const [bytes,type] of [[png,'image/png'],[Buffer.from([255,216,255]),'image/jpeg'],[webp(),'image/webp'],[webp('VP8L',1,1),'image/webp'],
    [webp('VP8X',2),'image/webp'],[webp('JUNK'),'image/webp'],[webp('VP8 ',1),'image/webp'],[png.subarray(0,7),'image/png'],
    [Buffer.from([255,216]),'image/jpeg'],[Buffer.alloc(20),'image/webp']])cases.push(media(bytes,type));
  const valid=media(png,'image/png');
  for(const [path,values] of [[['object','id'],['other']], [['object','contractKey'],['chat-bridge/image-webp']],
    [['revision','revision'],[2]], [['revision','contractVersion'],['1.3.0']], [['revision','mediaType'],['image/jpeg']],
    [['revision','contentHash'],['sha256:'+'b'.repeat(64)]], [['revision','byteLength'],[7]], [['content','encoding'],['json']],
    [['content','value'],['!'+png.toString('base64'),png.toString('base64').replace(/=+$/,''),png.toString('base64')+'\n',Buffer.alloc(8).toString('base64')]]]){
    for(const value of values){const c=structuredClone(valid);c.response[path[0]][path[1]]=value;cases.push(c);}
  }
  cases.push({...structuredClone(valid),caller:'mallory'});
  const textOnly=structuredClone(valid);textOnly.request.payload.images=[];cases.push(textOnly);
  const multiple=structuredClone(valid);multiple.request.payload.images.push(structuredClone(multiple.request.payload.images[0]));cases.push(multiple);
  const outage=structuredClone(valid);outage.failure=true;cases.push(outage);
  const rows=[];
  for(const c of cases){
    const calls=[],caller=c.caller??'alice';let result;
    const client={async request(method,params){calls.push({method,params});if(c.failure)throw new IvyError('service_unavailable','Synthetic read outage.','unknown');return structuredClone(c.response);}};
    try{result={value:await admittedNativeInput(admission,client,caller,c.request)};}
    catch(cause){const e=IvyError.from(cause);result={error:{code:e.code,message:e.message,outcome:e.outcome}};}
    rows.push({...c,definition,caller,calls,result});
  }
  process.stdout.write(JSON.stringify(rows));
  process.exit(0);
}
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
