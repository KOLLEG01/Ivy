// Execute unchanged application handlers. The injected SDK registration boundary
// captures callbacks/configuration; it does not qualify SDK or HTTP behavior.
import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
import {mcpIconPath} from '../../services/hive/src/mcp-branding.ts';
import {hiveMcpInstructions} from '../../instructions/hive-mcp.ts';
import {ivyDevMcpInstructions} from '../../instructions/ivy-dev-mcp.ts';
import {compareMcpTools} from '../../services/hive/src/mcp-presentation.ts';
const source=readFileSync(new URL('../../services/hive/src/mcp.ts',import.meta.url),'utf8');
const start=source.indexOf('export function mcpSurfaceBindings(');
if(start<0)throw Error('Original MCP handler boundary changed');
const code=stripTypeScriptTypes(source.slice(start).replace(/export (?=(?:async )?function)/g,''),{mode:'strip'});
const original=new Function('randomUUID','createMcpHandler','INVALID_PARAMS','METHOD_NOT_FOUND','ProtocolError','Server','toNodeHandler','hashJson','mcpDiscoveryResultBytes','IvyError','requireThat','directCall','directDescriptors','hasStructuredOutput','hiveMcpInstructions','ivyDevMcpInstructions','mcpIconPath','compareMcpTools',code+';return {handleMcp,mcpServerInfo,measuredListResult,failure};');
export async function request(input,dependencies){
 const {store,discovery,hashJson,IvyError,requireThat,directCall,directDescriptors,hasStructuredOutput}=dependencies;
 class ProtocolError extends Error{constructor(code,message,data){super(message);this.code=code;this.data=data;}}
 const calls=[],observations=[],envelopes=[];
 let result=null,protocolError=null,thrown=null,server,ids=0,ticks=0;
 const options={surface:'ivy',version:'1.2.3',publicBaseUrl:'https://hive.invalid/ivy/',resourceMetadataUrl:'https://hive.invalid/ivy/.well-known/oauth-protected-resource',...input.options};
 function fail(value){if(value.generic)throw Error('Synthetic foreign error.');if(value.protocol)throw new ProtocolError(value.protocol.code,value.protocol.message,value.protocol.data);throw new IvyError(value.code,value.message,value.outcome,value.details);}
 async function storage(message){
  calls.push(message);
  if(input.storageFailure?.action===message.action)fail(input.storageFailure);
  switch(message.action){
   case'mcp.catalog':return input.catalogBindings??discovery.mcpCatalog(message.name);
   case'mcp.cursor.create':return{cursor:store.cursor(message.identity,message.offset)};
   case'mcp.cursor.read':{
    const offset=store.readCursor(message.identity,message.cursor);
    requireThat(typeof offset==='number'&&Number.isSafeInteger(offset)&&offset>=0,'invalid_cursor','Invalid MCP discovery boundary.');
    return{offset};
   }
   default:throw Error('Unexpected private MCP action');
  }
 }
 const body=input.body??{jsonrpc:'2.0',id:'request',method:input.mode==='call'?'tools/call':'tools/list',params:{}};
 const realNow=Date.now;
 const api=original(
  ()=>{const index=ids++;return input.ids?.[index]??`request-${index}`;},
  factory=>({server:factory(),async close(){}}),-32602,-32601,ProtocolError,
  class{constructor(info,configuration){this.info=info;this.configuration=configuration;this.handlers=new Map();server=this;}setRequestHandler(method,handler){this.handlers.set(method,handler);}},
  handler=>async()=>{
   if(input.mode==='info'){result=handler.server.info;return;}
   if(input.mode==='instructions'){result=handler.server.configuration.instructions;return;}
   const handlerName=input.mode==='call'?'tools/call':'tools/list';
   Date.now=()=>input.clock?.[ticks++]??input.clock?.at(-1)??realNow();
   try{result=await handler.server.handlers.get(handlerName)(body);}finally{Date.now=realNow;}
  },hashJson,512*1024,IvyError,requireThat,directCall,directDescriptors,hasStructuredOutput,
  hiveMcpInstructions,ivyDevMcpInstructions,mcpIconPath,compareMcpTools,
 );
 options.catalog=name=>storage({action:'mcp.catalog',...(name?{name}:{})});
 options.cursors={create:async(identity,offset)=>(await storage({action:'mcp.cursor.create',identity,offset})).cursor,read:async(identity,cursor)=>(await storage({action:'mcp.cursor.read',identity,cursor})).offset};
 options.observeCall=observation=>{observations.push(observation);if(input.observerFailure)fail(input.observerFailure);};
 try{
  if(input.mode==='failure')result=api.failure(input.error.generic?new Error('Foreign error.'):new IvyError(input.error.code,input.error.message,input.error.outcome,input.error.details),options.resourceMetadataUrl);
  else await api.handleMcp({}, {}, body, {},async(_context,envelope)=>{
   envelopes.push(envelope);
   if(input.invokeFailure)fail(input.invokeFailure);
   if(envelope.method==='discovery.instructions')return discovery.instructions();
   return input.reply??null;
  },options);
 }catch(error){
  result=null;
  if(error instanceof ProtocolError)protocolError={code:error.code,message:error.message,data:error.data};
  else thrown=IvyError.from(error).toWire();
 }finally{Date.now=realNow;}
 return{result,protocolError,thrown,calls,observations,envelopes,measured:input.mode==='list'&&result?api.measuredListResult(result,server.info):null};
}
