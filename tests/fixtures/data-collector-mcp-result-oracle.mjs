import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
import {IvyError} from '../../packages/contracts/src/errors.ts';
const client=stripTypeScriptTypes(readFileSync(new URL('../../packages/sdk/src/client.ts',import.meta.url),'utf8'),{mode:'transform'});
const responseSource=client.slice(client.indexOf('export function responseValue('),client.indexOf('export function baseUrl(')).replace('export function','function');
const responseValue=new Function('IvyError',responseSource+';return responseValue;')(IvyError);
const native=stripTypeScriptTypes(readFileSync(new URL('../../services/data-collector/src/native-mcp.ts',import.meta.url),'utf8'),{mode:'transform'});
const resultSource=native.slice(native.indexOf('export function mcpToolResult('),native.indexOf('export class NativeMcpSession')).replace('export function','function');
const mcpToolResult=new Function('IvyError','responseValue',resultSource+';return mcpToolResult;')(IvyError,responseValue);
const cases=[{}, {structuredContent:false},{structuredContent:0},{structuredContent:[]},{structuredContent:{value:1},content:[{type:'text',text:'2'}]},
 {structuredContent:null,content:[{type:'image',text:'4'},{type:'text',text:'not JSON'},{type:'text',text:'null'},{type:'text',text:'9'}]},
 {content:[{type:'text',text:'unparsed'}]}, {content:[{type:'text',text:1}]}, {content:[]}, {isError:true},
 ...['false','0','"text"','[1,2]','{"x":3}','"\ud800"','"\\\ud800"','"\\\\\ud800"','\ud800'].map(text=>({content:[{type:'text',text}]}))];
for(const code of ['authentication_required','provider_unavailable','invalid_frame','CAPITAL','bad-code','','a'.repeat(128),'a'.repeat(129),'\ud800'])
 for(const outcome of ['not_executed','unknown','completed','invalid']){
   const error={code:-32000,message:'private provider detail',data:{code,outcome,details:{private:'data'}}};
   for(const payload of [{structuredContent:{error}},{content:[{type:'text',text:JSON.stringify({error})}]}])cases.push({isError:true,...payload});
 }
for(const payload of [null,false,[],{error:null},{error:{code:4}},{error:{code:-32000,message:'m',data:{code:'safe',outcome:'unknown',unexpected:true}}}])cases.push({isError:true,structuredContent:payload});
process.stdout.write(JSON.stringify(cases.map(input=>{try{return{input,value:mcpToolResult(input),error:null};}catch(cause){const e=IvyError.from(cause);return{input,value:null,error:{code:e.code,message:e.message,outcome:e.outcome}};}})));
