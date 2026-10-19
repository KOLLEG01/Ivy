import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';

const main=readFileSync('services/task-board/src/main.ts','utf8');
const minimum=main.slice(main.indexOf('  const minimumTickIntervalMs'),main.indexOf('  const writeHealth'));
const scheduling=main.slice(main.indexOf('  const schedule ='),main.indexOf('  const handlers:'));
const ready=main.slice(main.indexOf("if (state.status === 'ready'"),main.indexOf("else if (['offline'"));
const Subject=new Function('Date','setTimeout','clearTimeout','settings',stripTypeScriptTypes(`
function create() {
let closed=false,startedGeneration=null,wakePending=false,lastTickStartedAt=0,timerAt=0,timer=null,activeWork=null;
let resolveTick=null;const starts=[];const service={ready:false};
const runtime={recovered:false,tick(){starts.push(Date.now());return new Promise(resolve=>{resolveTick=resolve;});}};
${minimum}${scheduling}
return {ready(value){service.ready=value;const state={status:value?'ready':'degraded',generation:7};${ready}},kick,
async finish(value){assertTick();runtime.recovered=value;const done=activeWork;resolveTick();await done;},
snapshot(){return{starts:[...starts],timerAt:timer?timerAt:null};}};
function assertTick(){if(!activeWork)throw Error('No active source tick');}
}
`,{mode:'transform'})+'\nreturn create();');
const rows=[],add=(name,intervalMs,actions)=>rows.push({name,intervalMs,actions});
for(const interval of [100,250,1000]) {
 add('startup recovery and ordinary cadence '+interval,interval,[['kick'],['ready',true],['ready',true],['kick'],['kick'],['finish',false],['advance',50],['timer'],['advance',250],['timer'],['finish',true],['advance',interval],['timer'],['finish',true]]);
 add('quiet recovered tick '+interval,interval,[['ready',true],['finish',true],['advance',interval],['timer'],['finish',true]]);
 add('late active wake '+interval,interval,[['ready',true],['advance',3000],['kick'],['kick'],['finish',true],['timer'],['finish',true]]);
 add('earlier kick replaces later timer '+interval,interval,[['ready',true],['finish',true],['advance',1],['kick'],['advance',1],['kick'],['advance',interval],['timer'],['finish',true]]);
 add('degraded timer consumed and same generation ready '+interval,interval,[['ready',true],['finish',true],['ready',false],['kick'],['advance',interval],['timer'],['ready',true],['kick'],['finish',true]]);
}
add('ready repeated during active tick',1000,[['ready',true],['ready',true],['finish',true],['advance',100],['ready',true],['kick'],['advance',150],['timer'],['finish',true]]);
add('wall clock moves back during idle timer',1000,[['ready',true],['finish',true],['advance',-2000],['kick'],['timer'],['advance',3000],['timer'],['finish',true]]);
add('wall clock moves forward during timer',1000,[['ready',true],['finish',true],['advance',5000],['kick'],['finish',true]]);
add('slow recovery completion can restart immediately',1000,[['ready',true],['advance',4000],['finish',false],['advance',50],['timer'],['finish',true]]);
add('pending wake before recovery delay',1000,[['ready',true],['advance',500],['kick'],['finish',false],['advance',50],['timer'],['finish',true]]);
add('degraded active completion retains timer',1000,[['ready',true],['ready',false],['kick'],['finish',true],['advance',1000],['timer'],['ready',true],['kick'],['finish',true]]);
for(const row of rows) {
 let now=1000000,id=0;const timers=new Map();
 const subject=Subject({now:()=>now},(fn,delay)=>{const token=++id;timers.set(token,{fn,at:now+delay});return token;},token=>timers.delete(token),{scheduler:{intervalMs:row.intervalMs}});
 const expected=[];
 for(const action of row.actions) {
  if(action[0]==='ready')subject.ready(action[1]);
  else if(action[0]==='kick')subject.kick();
  else if(action[0]==='finish')await subject.finish(action[1]);
  else if(action[0]==='advance')now+=action[1];
  else if(action[0]==='timer'){
   const next=[...timers.entries()].sort((a,b)=>a[1].at-b[1].at)[0];
   if(next){timers.delete(next[0]);next[1].fn();}
  }else throw Error('Unknown action');
  expected.push(subject.snapshot());
 }
 row.expected=expected;
 // Numbers and keys in these snapshots are ASCII and ordered identically to
 // canonical JSON; retain the exact body as well as its comparison hash.
 row.expectedHash='sha256:'+createHash('sha256').update(JSON.stringify(expected.map(v=>({starts:v.starts,timerAt:v.timerAt})))).digest('hex');
}
assert.equal(rows.length,21);
process.stdout.write(JSON.stringify(rows));
