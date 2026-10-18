import {existsSync,readFileSync,mkdtempSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,basename} from 'node:path';
import {buildInstructions} from '../../../tools/build/build-instructions.mjs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
const contracts=new URL('../../../packages/contracts/src/',import.meta.url);
const data=source=>'data:text/javascript;base64,'+Buffer.from(source).toString('base64');
const sdk=data([
 `export {hashJson} from '${new URL('canonical.ts',contracts)}';`,
 `export {validateChat} from '${new URL('chat-validation.ts',contracts)}';`,
 `export {checkedNativeContract} from '${new URL('checked-native-contract.ts',contracts)}';`,
].join('\n'));
registerHooks({resolve(s,c,n){
 if(s==='../../../packages/sdk/src/node.js')return{url:sdk,shortCircuit:true};
 if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}
 return n(s,c);
},load(u,c,n){
 if(u.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(u),'utf8'),{mode:'transform'}),shortCircuit:true};
 return n(u,c);
}});
const {mainInputOrigin,mainInstructions,mainThreadOptions,mainTurnContext}=await import('../../../services/chat-bridge/src/main-context.ts');
const {chatTurnOptions}=await import('../../../services/chat-bridge/src/native-plan.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const cases=[];
const add=(label,kind,values)=>cases.push(structuredClone({label,kind,...values}));
// The first cases reuse the existing Main restriction/context and collaboration
// preference cases in chat-whatsapp-native.test.ts, without a native producer.
add('existing-main-thread-policy','thread',{template:{developerInstructions:'Personal settings',config:{'mcp_servers.ivy.disabled_tools':['another_tool'],'features.apps':false}}});
add('existing-main-turn-context','turn',{template:{additionalContext:{personal:{kind:'untrusted',value:'saved'}}},origin:'service notification'});
const existing={collaborationMode:{mode:'default',settings:{model:'original',reasoning_effort:'xhigh',developer_instructions:'retained'}},cwd:'C:/main'};
add('existing-selected-preferences','options',{template:existing,options:{model:'selected',effort:'low',serviceTier:'fast'}});
add('existing-null-preferences','options',{template:existing,options:{model:null,effort:null}});
const optional=[undefined,null,false,0,'',true,42,'Retained\ud800',[],[null,'text',false],{}, {toString:null},['before',{toString:4}]];
const configs=[undefined,null,false,0,'ignored',[],{}, {'features.apps':false},
 {'mcp_servers.ivy.disabled_tools':'chat_bridge_send'},
 {'mcp_servers.ivy.disabled_tools':['chat_bridge_send','other','chat_bridge_send','other']},
 {'mcp_servers.ivy.disabled_tools':[false,false,null,null,0,-0,'0',true,true,[],[],{tool:'x'},{tool:'x'},'\ud800','\ud800']},
 JSON.parse('{"__proto__":{"retained":true},"mcp_servers.ivy.disabled_tools":["other"],"1":true,"0":false}'),
];
for(const [i,developerInstructions]of optional.entries())for(const[j,config]of configs.entries())add('thread-'+i+'-'+j,'thread',{template:{retained:{opaque:'\udfff'},...(developerInstructions===undefined?{}:{developerInstructions}),...(config===undefined?{}:{config})}});
for(const template of [null,false,4,'A😀',[],['one','two']])add('thread-receiver-'+JSON.stringify(template),'thread',{template});
for(const [i,additionalContext]of [...configs,{personal:{kind:'untrusted',value:'saved'},'ivy-main':{kind:'untrusted',value:'replace'}},JSON.parse('{"__proto__":{"kind":"retained"},"10":true,"2":false}')].entries()){
 for(const origin of ['conversation input','service \ud800 notice',''])add('turn-'+i+'-'+origin,'turn',{template:{model:'retained',...(additionalContext===undefined?{}:{additionalContext})},origin});
}
for(const template of [null,false,4,'A😀',[],['one','two']])add('turn-receiver-'+JSON.stringify(template),'turn',{template,origin:'fixture'});
for(const caller of ['alice','service','\ud800','😀']){
 for(const source of [undefined,null,{serviceNodeId:'source',object:{objectId:'opaque\udfff',revision:2}},JSON.parse('{"z":1,"a":2,"10":"ten","2":"two"}'),'source\n\ud800']){
  add('notice-origin-'+caller+'-'+JSON.stringify(source),'origin',{caller,request:{action:'notify',...(source===undefined?{}:{source})}});
 }
 for(const action of [undefined,'send','other',null])add('conversation-origin-'+caller+'-'+action,'origin',{caller,request:action===undefined?{}:{action,source:{unrelated:true}}});
}
add('origin-null-receiver','origin',{caller:'alice',request:null});
const modes=[undefined,null,false,true,'ignored',[],{}, {mode:'default'},
 ...[null,false,'ignored',[],{}, {model:'retained',reasoning_effort:'high',developer_instructions:'retain\ud800'},JSON.parse('{"__proto__":{"retain":true},"1":true}')].map(settings=>({mode:'default',settings}))];
const preferences=[undefined,null,false,4,'A😀',[],{}, {model:'selected',effort:'low',serviceTier:'fast'}, {model:null,effort:null}, {model:'',effort:''}, {model:false,effort:0}, {model:[],effort:{}}, {model:'\ud800',effort:'high',collaborationMode:{mode:'override'},unknown:{retain:true}}];
for(const[i,mode]of modes.entries())for(const[j,options]of preferences.entries())add('options-'+i+'-'+j,'options',{template:{model:'original',effort:'xhigh',cwd:'/fixture/main',...(mode===undefined?{}:{collaborationMode:mode})},...(options===undefined?{}:{options})});
for(const template of [null,false,4,'A😀',[],['one','two']])add('options-receiver-'+JSON.stringify(template),'options',{template,options:{model:'selected'}});
for(const c of cases){
 const before=structuredClone(c);
 try{
  const value=c.kind==='thread'?mainThreadOptions(c.template):c.kind==='turn'?mainTurnContext(c.template,c.origin):c.kind==='origin'?mainInputOrigin(c.caller,c.request):chatTurnOptions(c.template,c.options);
  c.expected={value,serialized:JSON.stringify(value)};
 }catch(error){const e=IvyError.from(error);c.expected={error:{code:e.code,message:e.message,outcome:e.outcome}};}
 // A compositor must not alter the caller's retained plan or preferences.
 if(JSON.stringify({...c,expected:undefined})!==JSON.stringify(before))throw Error('Original compositor mutated its fixture: '+c.label);
}
const output=mkdtempSync(join(tmpdir(),'ivy-chat-instructions-'));
let instructionAsset;
try{
 const files=buildInstructions({root:process.cwd(),outputRoot:output,component:'chat-bridge'});
 if(files.length!==2||!files.some(p=>basename(p)==='chat-main-instructions.json')||!files.some(p=>basename(p)==='chat-notice-prompt.json'))throw Error('Expected the selected Main and notice instruction assets.');
 const bytes=readFileSync(files.find(p=>basename(p)==='chat-main-instructions.json'));
 if(JSON.parse(bytes.toString('utf8'))!==mainInstructions)throw Error('The built asset differs from the original instruction literal.');
 instructionAsset=bytes.toString('base64');
}finally{rmSync(output,{recursive:true,force:true});}
process.stdout.write(JSON.stringify({instructions:mainInstructions,instructionAsset,cases}));
