import {writeFileSync} from 'node:fs';
process.stdin.setEncoding('utf8');
let input=''; for await (const chunk of process.stdin) input+=chunk;
const context=JSON.parse(input); context.signal=AbortSignal.timeout(context.timeoutMs);
if(context.mcp) {
const connection=context.mcp;
context.mcp={call:async(tool,args={})=>{
const response=await fetch(connection.url,{method:'POST',headers:{'content-type':'application/json',authorization:'Bearer '+connection.token},body:JSON.stringify({tool,arguments:args}),signal:context.signal});
const reply=await response.json();
if(!response.ok||reply.error) throw Object.assign(new Error('MCP request failed.'),{code:reply.error?.code??'provider_unavailable',outcome:reply.error?.outcome??'unknown'});
return reply.result;
}};
}
try { const {default:run}=await import('./task.mjs');
const output=JSON.stringify(await run(context));
if(typeof output!=='string'||Buffer.byteLength(output)>1000000) process.exit(79);
writeFileSync('result.json',output,{mode:0o600}); process.exit(0);
} catch (error) {
const code=["authentication_required","interaction_required","configuration_invalid","provider_unavailable"].indexOf(error?.code);
process.exit(code < 0 ? 1 : 80 + code); }
