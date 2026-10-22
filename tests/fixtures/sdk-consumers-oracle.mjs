import {registerHooks} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {mainChat,mainNotification}=await import('../../packages/sdk/src/main-chat.ts');
const {hostProject}=await import('../../packages/sdk/src/project-inventory.ts');
const {publishBrowserNotice,browserNoticeTopic}=await import('../../packages/sdk/src/browser-notice.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
// Moving the existing schema to a shared asset must preserve its exact JSON value/order.
const schema=JSON.parse(readFileSync(new URL('../../specs/schemas/browser-notice.schema.json',import.meta.url)));
assert.equal(createHash('sha256').update(JSON.stringify(schema)).digest('hex'),'cd177ce3fe46a04ac915294c1c14cca62acbe9d9a04d1bca58a34a80d32a31d5');
const rows=[];
const error=e=>({code:e.code??null,message:e.message,outcome:e.outcome??null});
async function add(input){
  const trace=[],steps=structuredClone(input.steps??[]),ids=[];
  const client={async request(method,params,options){
    params=structuredClone(params);
    if(method==='events.publish'){
      assert.match(params.mutationId,/^fixture-epoch:\d+:[0-9a-f-]{36}$/);
      if(!ids.includes(params.mutationId))ids.push(params.mutationId);
      params.mutationId='generated-'+ids.indexOf(params.mutationId);
    }
    trace.push({method,params,...(method==='inventory.list'?{timeout:options.timeoutMs}:{})});
    const step=steps.shift();assert.ok(step,'Unexpected request');assert.equal(method,step.method);
    if(step.error)throw new IvyError(step.error.code,step.error.message,step.error.outcome,step.error.details);
    return structuredClone(step.value);
  }};
  let result;
  try{
    let value;
    if(input.kind==='notification-checked'){
      try{const notification=mainNotification(input.value);result={ok:true,value:{notification,hash:hashJson(notification)}};}
      catch(e){result={ok:false,error:error(IvyError.from(e))};}
      rows.push({input:JSON.stringify(input),result:JSON.stringify({trace,result})});
      return;
    }
    if(input.kind==='main-checked'){
      try { result={ok:true,value:await mainChat(client)}; }
      catch(e) {
        const normalized=IvyError.from(e);
        result={ok:false,origin:e instanceof IvyError?'ivy':'boundary',error:{...error(normalized),...(normalized.details===undefined?{}:{details:normalized.details})}};
      }
      rows.push({input:JSON.stringify(input),result:JSON.stringify({trace,result})});
      return;
    }
    if(input.kind==='main')value=await mainChat(client);
    else if(input.kind==='project')value=await hostProject(client,input.hostId,input.projectId,{timeoutMs:1234});
    else if(input.kind==='notification')value=mainNotification(input.value);
    else if(input.kind==='topic')value=browserNoticeTopic(input.namespace);
    else{
      value=[];
      for(const publication of input.publications){
        try{await publishBrowserNotice(client,publication.namespace,publication.notice);value.push({ok:true});}
        catch(e){value.push({ok:false,error:error(e)});}
      }
    }
    result={ok:true,value};
  }catch(e){result={ok:false,error:error(e)};}
  rows.push({input:JSON.stringify(input),result:JSON.stringify({trace,result})});
}
const binding={qualifiedName:'chat.workspace',definitionHash:'sha256:'+'a'.repeat(64),definition:{interfaceVersion:'1.2.0',annotations:{readOnlyHint:true}}};
const page={items:[binding],provider:{node:{serviceNodeId:'chat-node'}},nextCursor:null};
const node={serviceNodeId:'chat-node',serviceName:'chat-bridge',principalId:'chat-owner',connected:true,synced:true,ready:true};
const workspace={expectedBridge:{serviceNodeId:'chat-node',principalId:'chat-owner',generation:7},nativeOwnerReady:true,channels:[{channel:{adapter:'whatsapp',recipientId:'retained'},status:'ready'}]};
function main(selectedNode=node,selectedWorkspace=workspace,selectedPage=page){return {kind:'main',steps:[{method:'tools.list',value:selectedPage},{method:'serviceNodes.get',value:selectedNode},{method:'tools.call',value:selectedWorkspace}]};}
await add(main());
for(const key of ['connected','synced','ready'])await add(main({...node,[key]:false}));
await add(main({...node,serviceName:'different'}));
await add(main(node,{...workspace,nativeOwnerReady:false}));
await add(main(node,{...workspace,expectedBridge:{...workspace.expectedBridge,principalId:'other'}}));
for(const channels of [[],[...workspace.channels,...workspace.channels],[{channel:{adapter:'email'}}]])await add(main(node,{...workspace,channels}));
await add(main(node,workspace,{...page,items:[{...binding,definition:{interfaceVersion:'2.0.0'}}]}));
await add(main(node,workspace,{...page,items:[]}));
const paged=main();paged.steps.unshift({method:'tools.list',value:{...page,items:[],nextCursor:'next'}});await add(paged);
for(let stage=0;stage<3;stage++){const input=main();input.steps[stage]={method:input.steps[stage].method,error:{code:'outcome_unknown',message:'lost response',outcome:'unknown'}};await add(input);}
// Keep the original instanceof catch observable after normalizing its outer error.
const checked=input=>add({...input,kind:'main-checked'});
await checked(main());
for(const outcome of ['not_executed','unknown','completed'])for(let stage=0;stage<3;stage++){
  const input=main();
  input.steps[stage]={method:input.steps[stage].method,error:{code:'internal_error',message:'coded\ud800-'+ 'x'.repeat(2100),outcome,details:{retained:['\udfff',null,{value:1}]}}};
  await checked(input);
}
for(const selectedNode of [null,{},false,3,'node'])await checked(main(selectedNode));
for(const selectedWorkspace of [null,{},false,3,'workspace'])await checked(main(node,selectedWorkspace));
for(const expectedBridge of [null,{},false,3,'bridge'])await checked(main(node,{...workspace,expectedBridge}));
for(const channels of [null,{},false,3,'','x','long',[],[null],[{}],[{channel:null}],[{channel:{}}],{length:0},{length:1},{length:1,0:{channel:{adapter:'whatsapp'}}},[...workspace.channels,...workspace.channels]])await checked(main(node,{...workspace,channels}));
for(const flag of [null,false,0,'',true,1,'ready',{},[]]){
  for(const key of ['connected','synced','ready'])await checked(main({...node,[key]:flag}));
  await checked(main(node,{...workspace,nativeOwnerReady:flag}));
}
await checked(main({...node,principalId:null},{...workspace,expectedBridge:{principalId:null}}));
await checked(main({...node,principalId:{}},{...workspace,expectedBridge:{principalId:{}}}));
// Later bad fields must stay unread after an earlier false && operand.
await checked(main({...node,connected:false},null));
await checked(main(node,{...workspace,expectedBridge:{principalId:'different'},channels:null}));
await checked(main(node,{...workspace,nativeOwnerReady:false,channels:null}));
for(const selectedPage of [null,{}, {items:null}, {items:{}}, {...page,items:[null,binding]}, {...page,items:[binding,null]}, {...page,items:[{...binding,definition:null}]}, {...page,items:[{...binding,definition:false}]}, {...page,provider:null}, {...page,provider:{node:null}}])await checked(main(node,workspace,selectedPage));
for(const nextCursor of [null,false,0,''])await checked(main(node,workspace,{...page,items:[],nextCursor}));
const checkedPaged=main();checkedPaged.steps.unshift({method:'tools.list',value:{...page,items:[],nextCursor:'next'}});await checked(checkedPaged);
for(const serviceNodeId of [null,false,7])await checked(main(node,workspace,{...page,provider:{node:{serviceNodeId}}}));
const noProviderId=main(node,workspace,{...page,provider:{node:{}}});await checked(noProviderId);
const noHash=main(node,workspace,{...page,items:[{qualifiedName:binding.qualifiedName,definition:binding.definition}]});await checked(noHash);
const project={nativeId:'project',name:'Name',paths:['/work'],source:'native',extra:{preserved:true}};
const item=value=>({resourceRef:{nativeId:'project'},summary:value});
for(const hostId of [undefined,'','host','\ud800']){
  await add({kind:'project',hostId,projectId:'project',steps:[{method:'inventory.list',value:{items:[item(project)],nextCursor:null}}]});
  await add({kind:'project',hostId,projectId:'project',steps:[{method:'inventory.list',value:{items:[],nextCursor:'next'}},{method:'inventory.list',value:{items:[{resourceRef:{nativeId:'other'},summary:project},item(null),item([]),item({...project,nativeId:'other'}),item({...project,name:3}),item({...project,paths:null}),item(project)],nextCursor:null}}]});
}
for(const nextCursor of [null,''])await add({kind:'project',projectId:'missing',steps:[{method:'inventory.list',value:{items:[item(project)],nextCursor}}]});
await add({kind:'project',projectId:'project',steps:[{method:'inventory.list',value:{items:[item({...project,name:'',paths:[null,123]})],nextCursor:'unneeded'}}]});
await add({kind:'project',projectId:'project',steps:[{method:'inventory.list',error:{code:'cancelled',message:'observation cancelled',outcome:'not_executed'}}]});
for(const value of [{action:'notify',operationId:'original',source:{taskId:'task'},text:'message',channel:{hidden:true},transport:'omit'}, {}, {action:null,operationId:'\ud800',source:['exact'],text:null}])await add({kind:'notification',value});
for(const original of [
  {action:'notify',operationId:'original',source:{taskId:'task'},text:'message'},
  {action:null,operationId:null,source:null,text:null},
  {action:9007199254740992,operationId:'\ud800',source:['exact'],text:'\udfff'},
  {action:['notify'],operationId:1,source:{toString:1,'__proto__':null},text:''},
])for(let mask=0;mask<16;mask++){
  const value={};Object.keys(original).forEach((key,i)=>{if(mask&(1<<i))value[key]=original[key];});
  value.omittedTransport={unsafe:9007199254740992};
  await add({kind:'notification-checked',value});
}
for(const value of [null,false,0,'','\ud800',[],[1],{action:'notify',operationId:'other'}])await add({kind:'notification-checked',value});
for(const depth of [126,127,128]){
  let value=null;for(let i=0;i<depth;i++)value=[value];
  await add({kind:'notification-checked',value:{action:value,operationId:'other'}});
  await add({kind:'notification-checked',value:{action:'notify',operationId:'other',source:null,text:null,omittedTransport:value}});
}
for(const namespace of ['agent','chat','task-board','','\ud800','a.b'])await add({kind:'topic',namespace});
const notice={title:'Title',body:'Body',tag:'tag',target:{uiId:'task-board-ui',fragment:'#/tasks/original'}};
const notices=[notice,{...notice,title:'a'.repeat(99)+'😀',body:'b'.repeat(299)+'😀',extra:'preserved'}, {...notice,title:'',body:'',tag:'different'}, {...notice,title:'\ud800',body:'\udfff'}, {...notice,title:'é'.repeat(101),body:'😀'.repeat(200)}];
await add({kind:'publish',publications:notices.map(notice=>({namespace:'fixture',notice})),steps:[{method:'system.status',value:{runtimeEpoch:'fixture-epoch'}},...notices.map(()=>({method:'events.publish',value:{sequence:1}}))]});
await add({kind:'publish',publications:[{namespace:'a',notice},{namespace:'b',notice},{namespace:'a',notice}],steps:[{method:'system.status',value:{runtimeEpoch:'fixture-epoch'}},...[1,2,3].map(()=>({method:'events.publish',value:{sequence:1}}))]});
await add({kind:'publish',publications:[{namespace:'a',notice},{namespace:'a',notice}],steps:[{method:'system.status',value:{runtimeEpoch:'fixture-epoch'}},{method:'events.publish',error:{code:'outcome_unknown',message:'lost publish reply',outcome:'unknown'}},{method:'events.publish',value:{sequence:1}}]});
await add({kind:'publish',publications:[{namespace:'a',notice},{namespace:'a',notice}],steps:[{method:'system.status',error:{code:'outcome_unknown',message:'lost epoch reply',outcome:'unknown'}},{method:'system.status',value:{runtimeEpoch:'fixture-epoch'}},{method:'events.publish',value:{sequence:1}}]});
process.stdout.write(JSON.stringify(rows));
