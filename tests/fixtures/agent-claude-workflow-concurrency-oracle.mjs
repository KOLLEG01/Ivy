// The frozen runtime and journal monitor own every transition. Only the first
// directory read and consumer callback are held to expose concurrent ordering.
import {readFileSync, mkdirSync, writeFileSync} from 'node:fs';
import {resolve, join} from 'node:path';
import assert from 'node:assert/strict';
const {root}=JSON.parse(readFileSync(0,'utf8'));
globalThis.__ivyClaudeFixture={home:root,nextId:()=>{throw Error('Unexpected ID')}};
const {NativeClaudeRuntime}=await import('./agent-claude-protocol-reference.mjs');
const delay=ms=>new Promise(r=>setTimeout(r,ms));
const gate=()=>{let release;const wait=new Promise(r=>release=r);return {wait,release}};
const until=async predicate=>{const end=Date.now()+8000;while(!predicate()){assert(Date.now()<end,'fixture timed out');await delay(5)}};
const rows=[];
for(const kind of ['shared-flush','failure-deferred','hidden-stop','late-terminal','failure-terminal','rejected-start']){
  const base=resolve(root,kind),dir=join(base,'subagents','workflows','run');mkdirSync(dir,{recursive:true});
  writeFileSync(join(dir,'journal.jsonl'),'\n'+JSON.stringify({type:'result',agentId:'abcd1234',result:'individual'})+'\n');
  writeFileSync(join(dir,'agent-abcd1234.jsonl'),JSON.stringify({message:{role:'user',content:'First task\nMore'}})+'\n');
  const io=gate(),callback=gate(),events=[];let reads=0,outcome=null,monitor;
  process.env.CLAUDE_CONFIG_DIR=base;
  const runtime=new NativeClaudeRuntime();
  const pending={context:{threadId:'thread',turnId:'turn',cwd:root,prompt:'fixture',outputFormat:null},handlers:{onEvent:async event=>{
    events.push(structuredClone(event));
    if(event.type==='tool_use'&&event.toolUseId==='workflow-agent:task:abcd1234'){
      if(kind==='shared-flush')await callback.wait;
      if(kind==='rejected-start')throw Error('fixture consumer rejected');
    }
  }},abort:new AbortController(),resolved:false,resolve:()=>outcome={success:true},reject:e=>outcome={error:{name:e.name,message:e.message}},assistantMessageId:null,streamedBlocks:new Map(),activeSubagents:new Set(),completedWorkflowTasks:new Set(),workflowToolUseIds:new Set(),workflowLaunches:new Map(),workflowTranscriptRoots:[base],skippedWorkflowTaskIds:new Set(),workflowTasks:new Map(),toolStartSeen:new Set(),structuredBuffer:'',pendingUserMessage:null,deferredResult:null,workflowFailure:null};
  const create=runtime.createWorkflowMonitor.bind(runtime);
  runtime.createWorkflowMonitor=(...args)=>{
    monitor=create(...args);const read=monitor.resolveTranscriptDir.bind(monitor);
    monitor.resolveTranscriptDir=async()=>{
      reads++;await io.wait;
      if(kind==='failure-deferred'||kind==='failure-terminal')throw Error('fixture journal read failed');
      return read();
    };
    return monitor;
  };
  runtime.turns.set('turn',pending);
  const send=message=>runtime.handleMessage(pending,message);
  const result=()=>send({type:'result',subtype:'success',result:'parent'});
  const terminal=()=>send({type:'system',subtype:'task_notification',task_id:'task',status:'completed',summary:'finished'});
  const assistant=text=>send({type:'assistant',message:{id:text,content:[{type:'text',text}]}});
  await send({type:'assistant',message:{id:'workflow',content:[{type:'tool_use',id:'launch',name:'Workflow',input:{}}]}});
  await send({type:'user',tool_use_result:{status:'async_launched',taskId:'task',runId:'run',transcriptDir:dir,workflowName:'Journal'},message:{content:[{type:'tool_result',tool_use_id:'launch',content:'launched'}]}});
  await send({type:'system',subtype:'task_started',task_type:'local_workflow',task_id:'task',tool_use_id:'launch'});
  await until(()=>reads===1);
  const checks={attachReturnedWhileReadHeld:reads===1&&!events.some(e=>e.toolUseId==='workflow-agent:task:abcd1234')};
  if(kind==='shared-flush'){
    const a=monitor.flush(),b=monitor.flush();await delay(30);checks.sharedRead=reads===1;
    await assistant('visible');io.release();await until(()=>events.some(e=>e.type==='tool_use'&&e.toolUseId==='workflow-agent:task:abcd1234'));
    await assistant('suppressed');callback.release();await Promise.all([a,b]);
    checks.suppressedDuringCallback=!events.some(e=>e.delta==='suppressed');await result();checks.waitsForTerminal=outcome===null;await terminal();
  }else if(kind==='hidden-stop'){
    await send({type:'system',subtype:'task_started',task_type:'local_workflow',task_id:'task',skip_transcript:true});
    checks.boundedStopBeforeReadReleased=reads===1;io.release();await monitor.flush();await delay(30);await result();
    checks.noLateAgents=!events.some(e=>e.toolUseId==='workflow-agent:task:abcd1234');
  }else{
    await result();checks.waitsForTerminal=outcome===null;
    if(kind==='late-terminal'||kind==='failure-terminal'){
      const done=terminal();await delay(30);checks.sharedRead=reads===1;io.release();await done;
    }else{io.release();await until(()=>outcome!==null)}
  }
  await until(()=>outcome!==null);await monitor.stop();
  rows.push({kind,checks,events,outcome});
}
process.stdout.write(JSON.stringify({rows}));
