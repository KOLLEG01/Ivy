import {registerHooks,createRequire} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath,pathToFileURL} from 'node:url';
const require=createRequire(import.meta.url);
registerHooks({resolve(specifier,context,next){
  if(specifier.endsWith('.js')&&specifier.startsWith('.')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  try{return next(specifier,context);}catch(error){if(!specifier.startsWith('.')&&!specifier.startsWith('/')&&!specifier.includes(':'))return next(pathToFileURL(require.resolve(specifier)).href,context);throw error;}
}});
const {validateRequest,validateResponseFrame}=await import('../../packages/contracts/src/core-validation.ts');
const {validateProviderCallFrame,validateServiceNotificationFrame,validateProviderNotificationFrame,validateEventAvailabilityFrame}=await import('../../packages/contracts/src/transport-validation.ts');
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
process.stdout.write(JSON.stringify(cases));
