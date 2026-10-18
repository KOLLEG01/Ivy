import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root=resolve(process.env.IVY_SECRETARY_ORACLE??'.local/build/secretary'),load=p=>import(pathToFileURL(resolve(root,p)));
const {SecretaryEngine,operationName}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,hashJson,deriveOperationId}=await load('packages/sdk/src/node.js');
const now=1800000000000,rootId='00000000-0000-4000-8000-000000000001',objectId='00000000-0000-4000-8000-000000000002',id=`fixture-epoch:${now}:workflow`,caller='caller';
Date.now=()=>now;
const request={operationId:id,value:{fixture:true}},key='secretary/public-operation';
const write={mutationId:deriveOperationId(id,['secretary-workflow',caller]),contractVersion:'1.4.0',references:{},create:{parentId:rootId,contractKey:'secretary/assignment',name:'assignment-fixture',ownerObjectId:null},content:{encoding:'json',value:{fixture:true}}};
const effect={objectId,revision:1};
const receipt={object:{id:objectId,parentId:rootId,contractKey:'secretary/assignment',name:'assignment-fixture'},revision:{revision:1}};
const row={operationId:id,callerPrincipalId:caller,action:'create_assignment',requestHash:hashJson(request),write,phase:'prepared',errorCode:null};
const step=(method,value)=>({method,value}),failure=(method,outcome)=>({method,error:{code:'fixture_failure',message:'Fixture failure.',outcome}});
const rows=[];
async function add(method,args,{saved=null,steps=[],expiredBefore=0,capacity=10000}={}){
 const input=structuredClone({method,args,saved,steps,expiredBefore,capacity}),pending=structuredClone(steps),trace=[];
 const client={async request(method,params,options){trace.push({method,params:structuredClone(params),timeout:options?.timeoutMs??35000});const next=pending.shift();assert.ok(next,`Unexpected ${method}`);assert.equal(next.method,method);if(next.error)throw new IvyError(next.error.code,next.error.message,next.error.outcome);return structuredClone(next.value);}};
 const store=new SecretaryStore(client,rootId,undefined,{count:capacity,bytes:256*1024*1024});
 if(expiredBefore)store.db.prepare('INSERT INTO journal_state VALUES(?,?)').run('expiredBefore',String(expiredBefore));
 if(saved)store.technicalCreate(key,operationName(caller,id),saved);
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:{identity:{scope:{secretaryId:'secretary',rootObjectId:rootId}},sources:[]},epoch:Promise.resolve('fixture-epoch')});
 let result;
 try{result={value:(await engine[method](...structuredClone(args)))??null};}catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
 const stored=store.technicalNamed(key,operationName(caller,id));
 const cleanup=stored?store.db.prepare('SELECT delete_after FROM technical_cleanup WHERE root_id=?').get(stored.pin.objectId)?.delete_after??null:null;
 const retained=stored?{value:stored.value,revision:stored.pin.revision}:null;store.close();
 rows.push({input,expected:{result,trace,remaining:pending.length,retained,cleanup}});
}
for(const value of [id,`other:${now}:workflow`,`fixture-epoch:${now+60000}:future`,`fixture-epoch:${now+60001}:future`,'fixture-epoch:0:old','invalid'])await add('validateOperationId',[value]);
for(const expiredBefore of [now-1,now,now+1])await add('validateOperationId',[id],{expiredBefore});
await add('workflowReceipt',[row],{steps:[step('objects.writeReceipt',null)]});
await add('workflowReceipt',[row],{steps:[step('objects.writeReceipt',receipt)]});
await add('workflowReceipt',[{...row,phase:'succeeded',effect}]);
await add('workflowReceipt',[{...row,write:{...write,mutationId:'other'}}]);
for(const [field,value] of [['contractKey','secretary/item'],['parentId','other'],['name','other']])await add('workflowReceipt',[row],{steps:[step('objects.writeReceipt',{...receipt,object:{...receipt.object,[field]:value}})]});
const update={...row,action:'update_assignment',write:{...write,objectId,expectedRevision:1}};delete update.write.create;
await add('workflowReceipt',[update],{steps:[step('objects.writeReceipt',receipt)]});
await add('workflowReceipt',[update],{steps:[step('objects.writeReceipt',{...receipt,object:{...receipt.object,id:'other'}})]});
await add('workflowReceipt',[{...row,action:'configure_execution'}],{steps:[step('objects.writeReceipt',{...receipt,object:{...receipt.object,contractKey:'secretary/configuration'}})]});
await add('saveWorkflow',[caller,request,'create_assignment',write],{steps:[step('objects.write',receipt)]});
await add('saveWorkflow',[caller,request,'create_assignment',null]);
await add('saveWorkflow',[caller,request,'create_assignment',write],{capacity:0});
for(const outcome of ['not_executed','unknown','completed']){
 await add('saveWorkflow',[caller,request,'create_assignment',write],{steps:[failure('objects.write',outcome)]});
 await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:row,steps:[step('objects.writeReceipt',null),failure('objects.write',outcome)]});
 await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:row,steps:[failure('objects.writeReceipt',outcome)]});
}
await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:row,steps:[step('objects.writeReceipt',receipt)]});
await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:row,steps:[step('objects.writeReceipt',null),step('objects.write',receipt)]});
await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:{...row,phase:'succeeded',effect}});
await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:{...row,phase:'failed',errorCode:'prior_failure'},steps:[step('objects.writeReceipt',null)]});
await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:{...row,phase:'failed',errorCode:'prior_failure'},steps:[step('objects.writeReceipt',receipt)]});
await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:{...row,phase:'failed',errorCode:null},steps:[step('objects.writeReceipt',null)]});
for(const change of [{operationId:'other'},{callerPrincipalId:'other'},{action:'update_assignment'},{requestHash:'sha256:'+'0'.repeat(64)}])await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:{...row,...change}});
await add('saveWorkflow',[caller,{...request,value:{fixture:false}},'create_assignment',null],{saved:row});
await add('saveWorkflow',[caller,request,'create_assignment',null],{saved:row,expiredBefore:now+1,steps:[step('objects.writeReceipt',receipt)]});
process.stdout.write(JSON.stringify({root:rootId,now,epoch:'fixture-epoch',caller,id,rows}));
