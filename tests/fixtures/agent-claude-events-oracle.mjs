// Actual frozen NativeClaudeRuntime message/consume methods. No SDK import,
// executable, account or provider is used by this translation-only oracle.
import {readFileSync,writeFileSync,mkdirSync,lstatSync,realpathSync} from 'node:fs';
import {resolve,dirname,relative,isAbsolute} from 'node:path';
import assert from 'node:assert/strict';
const spec=JSON.parse(readFileSync(0,'utf8')),root=resolve(spec.root);
assert.equal(realpathSync(root),root);assert(!lstatSync(root).isSymbolicLink());
globalThis.__ivyClaudeFixture={home:root,nextId:()=>{throw Error('Unexpected random ID')}};
const {NativeClaudeRuntime}=await import('./agent-claude-protocol-reference.mjs');
const replace=v=>typeof v==='string'?v.replaceAll('__ROOT__',root):Array.isArray(v)?v.map(replace):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,replace(v)])):v;
const rows=[];
for(const raw of spec.rows){
  const row=replace(raw),events=[];let outcome=null;
  const base=resolve(root,row.directory),child=relative(root,base);
  assert(child && child!=='..'&&!child.startsWith('..\\')&&!child.startsWith('../')&&!isAbsolute(child));mkdirSync(base,{recursive:true});
  for(const file of row.files??[]){const path=resolve(base,file.path),child=relative(base,path);assert(child&&!child.startsWith('..')&&!isAbsolute(child));mkdirSync(dirname(path),{recursive:true});writeFileSync(path,file.text);}
  for(const key of Object.keys(process.env))if(/^(?:CLAUDE|ANTHROPIC|HOME$)/.test(key))delete process.env[key];
  process.env.CLAUDE_CONFIG_DIR=base;
  const runtime=new NativeClaudeRuntime();
  const pending={context:row.context,handlers:{onEvent:async e=>{
    events.push(structuredClone(e));if(e.type===row.rejectEventType)throw Error('fixture consumer rejected');
  }},abort:new AbortController(),resolved:false,resolve:()=>outcome={success:true},reject:e=>outcome={error:{name:e.name,message:e.message}},assistantMessageId:null,streamedBlocks:new Map(),activeSubagents:new Set(),completedWorkflowTasks:new Set(),workflowToolUseIds:new Set(),workflowLaunches:new Map(),workflowTranscriptRoots:[base],skippedWorkflowTaskIds:new Set(),workflowTasks:new Map(),toolStartSeen:new Set(),structuredBuffer:'',pendingUserMessage:null,deferredResult:null,workflowFailure:null};
  pending.query=(async function*(){for(const step of row.steps){
    if(step.error)throw Error(step.error);
    if(step.poll){for(const task of pending.workflowTasks.values())await task.monitor?.flush();continue;}
    yield step.message;
  }})();
  runtime.turns.set(row.context.turnId,pending);
  await runtime.consume(pending);
  for(const task of pending.workflowTasks.values())await task.monitor?.stop();
  rows.push({events,outcome});
}
process.stdout.write(JSON.stringify({rows}));
