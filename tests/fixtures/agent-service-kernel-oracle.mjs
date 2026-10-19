import assert from 'node:assert/strict';
import {readFileSync,readdirSync} from 'node:fs';
import ts from 'typescript';
import {nativeBrowserNotice,inputBrowserNotice,serviceOwnsCompletion} from '../../dist/services/agent-manager/src/notifications.js';
const source=ts.createSourceFile('main.ts',readFileSync('services/agent-manager/src/main.ts','utf8'),ts.ScriptTarget.Latest,true);
let declaration,options;
function visit(node){
 if(ts.isVariableDeclaration(node)&&node.name.getText(source)==='handlers')declaration=node.initializer;
 if(ts.isNewExpression(node)&&node.expression.getText(source)==='ServiceClient')options=node.arguments[0];
 ts.forEachChild(node,visit);
}visit(source);assert(declaration&&options);
const handlers=declaration.properties.map(p=>p.name.text).sort();assert.equal(handlers.length,22);
const config={serviceNodeId:'fixture.agent',hostId:'fixture',publicBaseUrl:'http://127.0.0.1:9/ivy',credential:'fixture-only',version:'1.2.3',buildId:'sha256:'+'1'.repeat(64)};
const settings={nativeVersion:'0.154.0'},registry={namespaces:[]};
const js=ts.transpileModule('return '+options.getText(source),{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
const captured=Function('config','settings','registry','handlers','jsonHandlers',js)(config,settings,registry,{},{});
const catalogs=readdirSync('specs/native').filter(n=>n.startsWith('codex-')).flatMap(n=>readdirSync('specs/native/'+n).filter(f=>/^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(f)).map(f=>'specs/native/'+n+'/'+f));assert.equal(catalogs.length,7);
const notices=[];
for(const node of ['fixture.agent','space +%~','日本語😀','\ud800node'])for(const thread of ['thread','a/b?&= +~!','😀\udfff']){
 const query=new URLSearchParams({node,id:thread}).toString();
 for(const status of ['completed','failed','interrupted',null]){
  const event={method:'turn/completed',params:{threadId:thread,turn:{id:'turn',status}}};
  notices.push({kind:'native',node,thread,query,input:event,expected:nativeBrowserNotice(node,event)});
 }
 for(const [state,method] of [['pending','tool/requestUserInput'],['answered','tool/requestUserInput'],['pending','currentTime/read']]){
  const input={threadId:thread,identity:{serviceNodeId:node,epoch:'epoch',requestId:7},state,method};
  notices.push({kind:'input',node,thread,query,input,expected:inputBrowserNotice(input)});
 }
}
const serviceOwnership=[];
for(const caller of [null,'','human','workflow','agent']){
 const calls=[];let index=0;
 const pages=[{items:[{principalId:'agent',serviceName:'agent-manager'}],nextCursor:'second'}, {items:[{principalId:'workflow',serviceName:'task-board'}],nextCursor:null}];
 const expected=await serviceOwnsCompletion({request:async(method,params)=>{calls.push({method,params});return pages[index++];}},caller);
 serviceOwnership.push({caller,pages,expected,calls});
}
const states=[];
for(const closing of [false,true])for(const status of ['connecting','offline','stopped','ready'])for(const code of [null,'fixture_code']){
 const health=[],env={closing,process:{stderr:{write(){}}},writeHealth:async(...args)=>{health.push(args);},IvyError:{from:()=>({code:'internal_error'})}};
 const expression=options.properties.find(p=>p.name.getText(source)==='onState').initializer.getText(source);
 const observer=Function('env','with(env){return '+ts.transpileModule(expression,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText.replace(/;\s*$/,'')+'}')(env);
 observer({status,code,generation:7,phase:'fixture'});await Promise.resolve();
 states.push({closing,status,code,health});
}
console.log(JSON.stringify({config,identity:captured.identity,heartbeatMs:captured.heartbeatMs,handlers,catalogs,notices,serviceOwnership,states}));
