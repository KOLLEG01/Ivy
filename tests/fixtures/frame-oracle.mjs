import {registerHooks,createRequire} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath,pathToFileURL} from 'node:url';
const require=createRequire(import.meta.url);
let resolvingBare=false;
registerHooks({resolve(specifier,context,next){
  if(specifier.endsWith('.js')&&specifier.startsWith('.')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  try{return next(specifier,context);}catch(error){
    if(!resolvingBare&&!specifier.startsWith('.')&&!specifier.startsWith('/')&&!specifier.includes(':')){
      resolvingBare=true;
      try{return next(pathToFileURL(require.resolve(specifier)).href,context);}finally{resolvingBare=false;}
    }
    throw error;
  }
}});
const {validateRequest,validateResponseFrame}=await import('../../packages/contracts/src/core-validation.ts');
const {validateProviderCallFrame,validateServiceNotificationFrame,validateProviderNotificationFrame,validateEventAvailabilityFrame,validateToolRouting}=await import('../../packages/contracts/src/transport-validation.ts');
const cases=[];
for(const [kind,validate,base] of [
  ['request',validateRequest,{jsonrpc:'2.0',id:'r',method:'objects.read',params:{}}],
  ['response',validateResponseFrame,{jsonrpc:'2.0',id:'r',result:null}],
  ['response',validateResponseFrame,{jsonrpc:'2.0',id:'r',error:{code:-32000,message:'failed',data:{code:'outcome_unknown',outcome:'unknown'}}}],
  ['provider',validateProviderCallFrame,{jsonrpc:'2.0',id:'r',method:'provider.invoke',params:{qualifiedName:'provider.tool',arguments:null,definitionHash:'sha256:'+'a'.repeat(64),generation:1,callerPrincipalId:'caller'}}],
  ['service-notification',validateServiceNotificationFrame,{jsonrpc:'2.0',method:'service.notification',params:{namespace:'fixture',name:'notice',version:'1.0.0',payload:null}}],
  ['provider-notification',validateProviderNotificationFrame,{jsonrpc:'2.0',method:'notifications.provider',params:{namespace:'fixture',name:'notice',version:'1.0.0',payload:null,serviceNodeId:'provider',generation:1}}],
  ['event-availability',validateEventAvailabilityFrame,{jsonrpc:'2.0',method:'events.available',params:{throughSequence:1}}],
]){
  const variants=[base,null,[],true,{}, {...base,extra:true}];
  for(const key of Object.keys(base)){const missing={...base};delete missing[key];variants.push(missing);for(const value of [null,false,0,-1,9007199254740992,'','\ud800',[],{},'x'.repeat(2049)])variants.push({...base,[key]:value});}
  if(base.error) for(const outcome of ['unknown','completed','not_executed',['completed'],['unknown'],null,{},false])variants.push({...base,error:{...base.error,data:{...base.error.data,outcome}}});
  if(base.params&&kind!=='request'){
    variants.push({...base,params:{...base.params,unexpected:true}});
    for(const key of Object.keys(base.params)){
      const missing={...base.params};delete missing[key];variants.push({...base,params:missing});
      for(const value of [null,false,0,-1,1.5,9007199254740991,9007199254740992,'','\ud800',[],{},'x'.repeat(2049),'a\n','1.0.0\n','0.01.0','0.0.0','10.99.100','a-b0'])variants.push({...base,params:{...base.params,[key]:value}});
    }
  }
  for(const input of variants){const row={kind,input:JSON.stringify(input)};try{validate(input);row.ok=true;}catch(error){row.code=error.code;row.message=error.message;}cases.push(row);}
}
const common={qualifiedName:'sample.read',expectedDefinitionHash:'sha256:'+'a'.repeat(64),arguments:{}};
const reference={serviceNodeId:'node',namespace:'sample',kind:'thread',nativeId:'x'.repeat(300)};
const malformed=[null,false,0,1,[],{},'', 'x'.repeat(257),'😀'.repeat(129),'\ud800'.repeat(257)];
const strings=['',' ', '\0', '\ud800', 'x'.repeat(64),'x'.repeat(65),'x'.repeat(256),'x'.repeat(257),'😀'.repeat(128),'😀'.repeat(129),'\ud800'.repeat(256),'a\n','a\r','a\r\n','a\u2028','a\u2029'];
for(const discovery of [false,true]){
  const base={...common,...(discovery?{serviceName:'sample-service'}:{})};
  const variants=[base,null,[],true,0,'value',{}, {...base,extra:true}, {...base,serviceName:'valid-service',resourceRef:reference}];
  for(const key of ['qualifiedName','arguments','expectedDefinitionHash','operationId','expectedCallerPrincipalId','serviceNodeId','serviceName','hostId']){
    const missing={...base};delete missing[key];variants.push(missing);
    for(const value of [...malformed,...strings])variants.push({...base,[key]:value});
  }
  for(const value of [null,false,0,'',[],{},[1],{nested:true}])variants.push({...base,arguments:value});
  for(const hash of ['sha256:'+'0'.repeat(64),'sha256:'+'f'.repeat(64),'sha256:'+'A'.repeat(64),'SHA256:'+'a'.repeat(64),'sha256:'+'a'.repeat(63),'sha256:'+'a'.repeat(65),'sha256:'+'a'.repeat(64)+'\n'])variants.push({...base,expectedDefinitionHash:hash});
  for(const name of ['a','a0-','-a','A','0a','a_b','é','a'.repeat(64),'a'.repeat(65),'😀','a\n'])variants.push({...base,serviceName:name});
  variants.push({...base,serviceNodeId:'top',hostId:'host',serviceName:'sample-service',resourceRef:reference});
  for(const value of [null,false,0,'',[],{},reference,{...reference,extra:true}])variants.push({...base,resourceRef:value});
  for(const [key,maximum]of [['serviceNodeId',256],['namespace',64],['kind',64],['nativeId',1024]]){
    const missing={...reference};delete missing[key];variants.push({...base,resourceRef:missing});
    for(const value of [...malformed,'x'.repeat(maximum),'x'.repeat(maximum+1),'😀'.repeat(maximum/2),'😀'.repeat(maximum/2)+'x','\ud800'.repeat(maximum)])variants.push({...base,resourceRef:{...reference,[key]:value}});
  }
  variants.push({...base,qualifiedName:'',resourceRef:null});
  if(discovery)variants.push({...common,resourceRef:null});
  for(const input of variants){
    const row={kind:discovery?'discovery-routing':'tool-routing',input:JSON.stringify(input)};
    try{validateToolRouting(input,discovery);row.ok=true;}catch(error){row.code=error.code;row.message=error.message;row.wire=JSON.stringify(error.toWire());}
    cases.push(row);
  }
}
for(const input of ['{"qualifiedName":"first","qualifiedName":"","expectedDefinitionHash":"sha256:'+'a'.repeat(64)+'","arguments":null}', '{"qualifiedName":"","qualifiedName":"last","expectedDefinitionHash":"sha256:'+'a'.repeat(64)+'","arguments":null}']){
  const row={kind:'tool-routing',input};try{validateToolRouting(JSON.parse(input),false);row.ok=true;}catch(error){row.code=error.code;row.message=error.message;row.wire=JSON.stringify(error.toWire());}cases.push(row);
}
process.stdout.write(JSON.stringify(cases));
