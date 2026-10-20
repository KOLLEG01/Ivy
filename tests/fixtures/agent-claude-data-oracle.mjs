import {reference} from './agent-claude-data-reference.mjs';
import {mkdirSync,readFileSync,existsSync} from 'node:fs';
import {join} from 'node:path';
let input='';for await(const bytes of process.stdin)input+=bytes;const spec=JSON.parse(input);
const state={now:spec.now??1700000000123,ids:spec.ids??[],idCalls:0,clockCalls:0,style:spec.style??'windows'};
const api=reference(spec.root,state),threads=new Map(Object.entries(spec.threads??{}));
const lookup=id=>threads.get(id)??null;
const file=spec.kind==='project'?'projects.json':'rate-limits.json';
const snapshot=()=>existsSync(join(spec.root,file))?readFileSync(join(spec.root,file),'utf8'):null;
const capture=fn=>{try{const value=fn();return {result:value??null};}catch(error){return {error:{name:error.name,message:error.message}};}};
if(spec.kind==='models'){
 console.log(JSON.stringify({records:spec.rows.map(row=>capture(()=>api.runtimeModelList(row.models,row.model,row.effort)))}));
}else{
 let store=spec.kind==='project'?new api.ProjectStore(lookup):new api.RateLimitStore();
 const records=[];
 for(const step of spec.steps){
  if(step.now!==undefined)state.now=step.now;
  const result=capture(()=>{
   if(step.op==='reopen'){store=spec.kind==='project'?new api.ProjectStore(lookup):new api.RateLimitStore();return;}
   if(step.op==='threads'){threads.clear();for(const [id,value]of Object.entries(step.value))threads.set(id,value);return;}
   if(step.op==='blockWrite'){mkdirSync(join(spec.root,file+'.tmp'));return;}
   if(spec.kind==='project'){
    switch(step.op){case'create':return store.create(step.params);case'update':return store.update(step.params);case'delete':return store.delete(step.id);case'read':return store.read(step.id);case'list':return store.list(step.params??{});case'bind':return store.bind(step.thread,step.id);case'projectId':return store.projectId(step.thread);}
   }else{
    switch(step.op){case'event':return store.recordEvent(step.value,state.now);case'usage':return store.recordUsage(step.value,state.now);case'due':return store.usageDue(state.now,step.interval??60000);case'read':return store.read(step.seconds??Math.floor(state.now/1000));}
   }
   throw Error('Unknown fixture operation');
  });records.push({op:step.op,...result,file:snapshot(),idCalls:state.idCalls,clockCalls:state.clockCalls});
 }
 console.log(JSON.stringify({records}));
}
