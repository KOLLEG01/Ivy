import assert from 'node:assert/strict';
import {load,itemId,message,definition,policy} from './inbox-fixtures.mjs';
const {triagePrompt,triageNativeParams,triageOutputSchema,triageToolConfiguration}=await load('services/secretary/src/triage-prompt.js');
const {secretaryNotificationPolicy}=await load('instructions/secretary-notification-policy.js');
const {IvyError,canonical}=await load('packages/sdk/src/node.js');
const settings={schemaVersion:1,target:{serviceNodeId:'fixture.agent'},model:'model',effort:'medium',instructions:'Explicit rules.',threadCwd:'/workspace',maximumConcurrent:1};
const input={item:{objectId:itemId,revision:2},source:definition,message,contentScope:'captured_text_or_caption',media:null,policy};
const rows=[];
function add(label,value=input,config=settings,media=[],kind='prompt'){
 let expected;
 try {
  const prompt=triagePrompt(value,config);
  expected={value:kind==='params'?triageNativeParams(config,prompt,media):prompt};
 } catch(cause) {const e=IvyError.from(cause);expected={code:e.code,message:e.message,outcome:e.outcome};}
 rows.push({label,input:value,settings:config,media,kind,expected});
}
add('exact prompt');
for(const text of ['', 'Preserve\n\tquotes " and slashes \\', '\ud800', '\udc00','é😀中文\0']){
 add('message and instructions '+JSON.stringify(text),{...input,message:{...message,text,title:text}}, {...settings,instructions:text});
 add('native parameters '+JSON.stringify(text),input,{...settings,instructions:text,threadCwd:'/workspace/'+text,model:'model-'+text},[{type:'text',text},{type:'image',url:'data:image/png;base64,AA==',opaque:{text}}],'params');
}
for(const instructions of [null,0,true,[],['one',null,'two'],{opaque:true},{toString:null}])add('instruction coercion '+JSON.stringify(instructions),input,{...settings,instructions});
const missing={...settings};delete missing.instructions;add('missing instructions',input,missing);
for(const config of [null,[],true,'string'])add('settings boundary '+JSON.stringify(config),input,config);
for(const value of [null,[],true,'direct text',{'10':'ten','2':'two',a:'first',z:'last','😀':'emoji','\ud800':'unpaired'}])add('canonical input '+JSON.stringify(value),value);
const base=Buffer.byteLength(canonical({...input,message:{...message,text:''}}));
for(const delta of [-1,0,1])add('UTF8 input boundary '+delta,{...input,message:{...message,text:'x'.repeat(262144-base+delta)}});
add('verified document coverage',{...input,contentScope:'verified_media',media:{coverage:{objectId:itemId,revision:1},coverageContract:'media/coverage',coverageHash:'sha256:'+'a'.repeat(64),gap:'partial',attachments:[{representation:'document_text',sections:['page1'],limitations:['missing images']}]}});
for(const effort of ['low','medium','high','xhigh'])add('native effort '+effort,input,{...settings,effort},[],'params');
const first=triageOutputSchema();first.properties.urgency.enum.push('changed');
assert.ok(!triageOutputSchema().properties.urgency.enum.includes('changed'));
assert.ok(rows.find(r=>r.label==='UTF8 input boundary 0').expected.value);
assert.equal(rows.find(r=>r.label==='UTF8 input boundary 1').expected.code,'content_too_large');
process.stdout.write(JSON.stringify({notificationPolicy:secretaryNotificationPolicy,schema:triageOutputSchema(),toolConfiguration:triageToolConfiguration,rows}));
