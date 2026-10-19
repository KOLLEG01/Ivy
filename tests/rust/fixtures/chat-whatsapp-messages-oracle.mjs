import {readFileSync,writeFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
const path=new URL('../../../services/chat-bridge/src/whatsapp/messages.ts',import.meta.url);
const source=readFileSync(path,'utf8');
const {translator,keys}=await import('data:text/javascript;base64,'+Buffer.from(stripTypeScriptTypes(source+'\nexport const keys=Object.keys(english);',{mode:'strip'})).toString('base64'));
const rows=[];
for(const key of keys){
 const names=[...new Set([...translator()(key).matchAll(/\{([a-z]+)\}/gi)].map(m=>m[1]))];
 const parameters=[undefined,{},Object.fromEntries(names.map(name=>[name,'$& $$ {code} 😀\ud800'])),Object.fromEntries(names.map(name=>[name,-0])),Object.fromEntries(names.map(name=>[name,1e-7])),Object.fromEntries(names.map(name=>[name,1e21])),Object.fromEntries(names.map(name=>[name.toUpperCase(),'wrong case']))];
 for(const language of [undefined,'en','de','fr'])for(const values of parameters){
  rows.push({key,...(language===undefined?{}:{language}),...(values===undefined?{}:{parameters:values}),expected:translator(language)(key,values)});
 }
}
const corpus={keys:keys.length,rows,boundary:'Unchanged original translator; all original keys, instance-local language/default/fallback and actual JavaScript interpolation. No account/runtime transport.'};
const output=JSON.stringify(corpus);
if(process.env.IVY_CHAT_MESSAGES_ORACLE_CAPTURE)writeFileSync(process.env.IVY_CHAT_MESSAGES_ORACLE_CAPTURE,output);
process.stdout.write(output);
