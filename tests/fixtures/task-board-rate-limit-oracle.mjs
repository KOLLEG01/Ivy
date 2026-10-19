import{readFileSync,existsSync}from'node:fs';
import{stripTypeScriptTypes,registerHooks}from'node:module';
import{fileURLToPath}from'node:url';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const{canonical,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const{IvyError}=await import('../../packages/contracts/src/errors.ts');
const source=readFileSync('services/task-board/src/runtime/native-rate-limit.ts','utf8');
const checks=readFileSync('services/task-board/src/runtime/checks.ts','utf8'),sameSource=checks.slice(checks.indexOf('export const same'),checks.indexOf(';',checks.indexOf('export const same'))+1).replace('export ','');
const nativeRecord=v=>v!==null&&typeof v==='object'&&!Array.isArray(v)?v:{},nativeArray=v=>Array.isArray(v)?v:[],nativeString=v=>typeof v==='string'?v:'';
const num=n=>Number.isNaN(n)?{number:'NaN'}:n===Infinity?{number:'Infinity'}:n===-Infinity?{number:'-Infinity'}:Object.is(n,-0)?{number:'-0'}:n;
const failure=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome,details:e.details??null};};
const pure=[],guards=[],observed='2026-03-29T00:20:00.000Z';
const turn=(message="You've hit your usage limit.",code,more={})=>({status:'failed',error:{message,...(code===undefined?{}:{codexErrorInfo:code})},...more});
const add=(name,value=turn(),at=observed)=>pure.push({name,turn:value,observedAt:at});
for(const status of['completed','inProgress','interrupted',null])add('nonfailure '+status,{...turn(),status});
for(const error of[null,{},[],true,{message:15},{message:['Rate limit reached']}])add('invalid error '+JSON.stringify(error),{status:'failed',error});
for(const message of["You've hit your usage limit.","You’ve hit your session limit.","YOU'VE HIT YOUR WEEKLY LIMIT!",'rate limit reached','Rate Limit Exceeded.','Claude Sonnet limit reached','Claude a_b 2 limit reached','Claude '+'x'.repeat(64)+' limit reached'])add('recognized '+message,turn(message));
for(const message of['warning: rate limit reached','you have hit your usage limit',"You've hit your usage limitless",'rate limit exceededed','Claude  limit reached','Claude '+'x'.repeat(65)+' limit reached','Claude é limit reached','Selected model is at capacity?'])add('unrecognized '+message,turn(message));
for(const message of['Selected model is at capacity','SELECTED MODEL IS AT CAPACITY. later','Selected model is at capacity!','Selected model is at capacity\n'])add('capacity '+JSON.stringify(message),turn(message));
add('capacity code overrides message',turn('', 'serverOverloaded'));
add('usage code overrides message',turn('arbitrary', 'usageLimitExceeded'));
add('unmatched UTF16 message',turn('arbitrary\ud800', 'usageLimitExceeded'));
for(const completedAt of[0,-1,1774743500,1774744000,1774743600.125,-0.0005,'1774743500',null])add('completed time '+JSON.stringify(completedAt),turn('', 'serverOverloaded',{completedAt}));
for(const reset of['2026-03-29T01:00:00Z','2026-03-29t01:00:00z','2026-03-29T02:00:00+01:00','2026-03-29T01:00:00.123456Z','2026-03-29T00:00:00Z','2026-02-30T01:00:00Z','2026-13-29T01:00:00Z','2026-03-29T01:00:00+24:00'])add('ISO reset '+reset,turn('Rate limit reached; resets at '+reset));
add('ISO has priority over earlier clock',turn('Rate limit reached; reset 1:00 (UTC), later resets 2026-03-29T03:00:00Z'));
add('invalid first ISO does not select later clock',turn('Rate limit reached; reset 2026-13-29T03:00:00Z; resets 1:00 (UTC)'));
add('nonmatching reset precedes valid clock',turn('Rate limit reached; reset unknown; resets 2:00 (UTC)'));
for(const [label,clock,at]of[
 ['UTC today','1:00 (UTC)',observed],['UTC tomorrow','0:10 (UTC)',observed],['grace same day','0:19 (UTC)',observed],
 ['12am','12:00 am (UTC)',observed],['12pm','12:00 PM (UTC)',observed],['afternoon','1:02 pm (America/New_York)',observed],
 ['spring gap','2:30 (Europe/Berlin)',observed],['autumn repeat','2:30 (Europe/Berlin)','2026-10-25T00:00:00.000Z'],
 ['half hour','8:00 (Asia/Kolkata)',observed],['quarter hour','8:00 (Asia/Kathmandu)',observed],['date line','8:00 (Pacific/Kiritimati)',observed],
 ['numeric offset','8:00 (+02:30)',observed],['bad zone','8:00 (Unknown/Zone)',observed],['unmatched zone','8:00 (UTC\ud800)',observed],
 ['hour zero','0:10 am (UTC)',observed],['hour13 period','13:00 pm (UTC)',observed],['hour24','24:00 (UTC)',observed],['minute60','2:60 (UTC)',observed],
 ['empty zone','2:00 ()',observed],['unclosed zone','2:00 (UTC',observed],['single minute','2:0 (UTC)',observed],['three hour digits','123:00 (UTC)',observed],
 ['clock word prefix','2:00 (UTC)',observed],['multiline whitespace','2:00\nPM\t(UTC)',observed],['month rollover','0:10 (UTC)','2026-01-31T23:50:00.000Z'],
 ['year99','1:00 (UTC)','0099-01-01T00:00:00.000Z'],['BC Intl year','1:00 (UTC)','0000-01-01T00:00:00.000Z'],
 ])add(label,turn('Rate limit reached; resets at '+clock),at);
add('embedded word reset does not match',turn('Rate limit reached; presets 2:00 (UTC)'));
add('literal at spacing',turn('Rate limit reached; resets\tat 2:00 (UTC)'));
add('Unicode whitespace',turn('Rate limit reached; RESET\u00a02:00 (UTC)'));
add('session stale fallback',turn("You've hit your session limit; resets 0:10 (UTC)"));
add('five hour stale fallback',turn('Rate limit exceeded; five hour reset 0:10 (UTC)'));
add('session exact5 hours',turn("You've hit your session limit; resets 5:20 (UTC)"));
add('weekly permits next day',turn("You've hit your weekly limit; resets 0:10 (UTC)"));
add('invalid observed capacity',turn('', 'serverOverloaded'),'not a date');
add('invalid observed recognized',turn(),'not a date');
add('invalid observed unrecognized',turn('other'),'not a date');
add('maximum time overflow',turn('', 'serverOverloaded'),'+275760-09-13T00:00:00.000Z');
add('minimum time capacity',turn('', 'serverOverloaded'),'-271821-04-20T00:00:00.000Z');

const pin=(objectId,revision=1)=>({objectId,revision}),primary={kind:'thread',namespace:'codex',serviceNodeId:'native-agent',nativeId:'thread'},runPin=pin('run',2),resultPin=pin('result',3);
const baseTask={pin:pin('self',4),value:{blocker:null,waiting:{reason:'user',detail:'Execution failed; see the ticket comment.'},primaryResourceRef:primary,lastRun:runPin,latestResult:resultPin}};
const baseRun={pin:runPin,value:{taskId:'self',phase:'failed',externalOutcome:{state:'failed',code:'native_failed'},cancellation:null,turnId:'turn',nativeEpoch:'epoch',result:resultPin,primaryResourceRef:primary,finishedAt:observed}};
const baseResult={pin:resultPin,value:{content:{summary:'Native turn failed.'}}};
const inspection={kind:'found',metadata:{reply:{result:{thread:{status:{type:'idle'}}}}},turns:{reply:{result:{data:[{id:'turn',...turn('', 'usageLimitExceeded') }]}},observedAt:'2026-03-29T00:21:00.000Z'}};
const guard=(name,change=()=>{})=>{const row={name,task:structuredClone(baseTask),run:structuredClone(baseRun),result:structuredClone(baseResult),inspection:structuredClone(inspection),question:false};change(row);guards.push(row);};
guard('exact failed native turn');
for(const [name,mutate]of[
 ['blocker',r=>r.task.value.blocker={}],['wrong waiting reason',r=>r.task.value.waiting.reason='host'],['wrong detail',r=>r.task.value.waiting.detail='other'],
 ['no waiting',r=>r.task.value.waiting=null],['no primary',r=>r.task.value.primaryResourceRef=null],['wrong namespace',r=>r.task.value.primaryResourceRef.namespace='other'],
 ['wrong kind',r=>r.task.value.primaryResourceRef.kind='project'],['no Run',r=>r.task.value.lastRun=null],['current unanswered question',r=>r.question=true],
 ['question provider refusal',r=>r.fault='question'],['Run read failure',r=>r.fault='run'],['wrong task',r=>r.run.value.taskId='other'],['wrong phase',r=>r.run.value.phase='completed'],
 ['wrong outcome code',r=>r.run.value.externalOutcome.code='other'],['wrong outcome state',r=>r.run.value.externalOutcome.state='unknown'],['cancellation',r=>r.run.value.cancellation={}],
 ['no turn',r=>r.run.value.turnId=null],['no epoch',r=>r.run.value.nativeEpoch=null],['no result',r=>r.run.value.result=null],['not latest result',r=>r.task.value.latestResult=pin('result',4)],
 ['different primary',r=>r.run.value.primaryResourceRef={...r.run.value.primaryResourceRef,nativeId:'other'}],['Result read failure',r=>r.fault='result'],
 ['summary prose ignored',r=>r.result.value.content.summary='We will retry later.'],['usage summary eligible',r=>r.result.value.content.summary='Rate limit reached'],
 ['capacity summary eligible',r=>r.result.value.content.summary='Selected model is at capacity.'],['summary missing',r=>delete r.result.value.content.summary],
 ['inspection failure',r=>r.fault='inspect'],['inspection pending',r=>r.inspection={kind:'pending',search:pin('search')}],['inspection absent',r=>r.inspection={kind:'absent'}],
 ['metadata native error',r=>r.inspection.metadata.reply={error:{code:1}}],['turns native error',r=>r.inspection.turns.reply={error:{code:2}}],
 ['active thread',r=>r.inspection.metadata.reply.result.thread.status.type='active'],['no matching turn',r=>r.inspection.turns.reply.result.data[0].id='different'],
 ['completed matching turn',r=>r.inspection.turns.reply.result.data[0].status='completed'],['unrecognized failed matching turn',r=>r.inspection.turns.reply.result.data[0].error={}],
 ['Run finished time fallback',r=>r.run.value.finishedAt=null],['completedAt original failure',r=>r.inspection.turns.reply.result.data[0].completedAt=1774743500],
 ['first matching turn selected',r=>r.inspection.turns.reply.result.data.unshift({id:'other',...turn('', 'serverOverloaded')})],
 ])guard(name,mutate);
const execute=async(row,kind)=>{
 const boundaries=[];
 const call=(name,args,work)=>{const b={kind:name,args:structuredClone(args)};boundaries.push(b);try{b.value=work();return b.value;}catch(e){b.error=failure(e);throw e;}};
 class ProbeDate extends Date{
  constructor(at){super(at);this.original=at;}
  static parse(value){return call('parse',[value],()=>num(Date.parse(value)));}
  static UTC(...args){return call('utc',[...args, ...Array(6-args.length).fill(0)],()=>num(Date.UTC(...args)));}
  toISOString(){return call('iso',[num(this.original)],()=>super.toISOString());}
 }
 // The source needs numeric Date results, while the transcript must retain NaN,
 // infinities and negative zero outside JSON's numeric domain.
 const decode=v=>v&&typeof v==='object'&&v.number?({NaN:NaN,Infinity:Infinity,'-Infinity':-Infinity,'-0':-0})[v.number]:v;
 const ParsedDate=new Proxy(ProbeDate,{get(target,key){if(key==='parse'||key==='UTC')return(...args)=>decode(target[key](...args));return Reflect.get(target,key);}});
 const IntlProbe={DateTimeFormat:class{
  constructor(locale,options){this.zone=options.timeZone;this.format=call('zone',[this.zone],()=>{const format=new Intl.DateTimeFormat(locale,options);return format;});boundaries.at(-1).value=true;}
  formatToParts(at){const parts=call('parts',[this.zone,num(at)],()=>Object.fromEntries(this.format.formatToParts(at).filter(p=>['year','month','day','hour','minute','second'].includes(p.type)).map(p=>[p.type,Number(p.value)])));return Object.entries(parts).map(([type,value])=>({type,value:String(value)}));}
 }};
 const fault=name=>{if(row.fault===name)throw new IvyError('fixture_failed','Refused '+name,'unknown',{name});};
 const question=(task,id)=>call('question',[task,id],()=>{fault('question');return row.question;});
 class Inspector{async find(pin){return call('inspect',[pin],()=>{fault('inspect');return row.inspection;});}}
 const subject=new Function('nativeRecord','nativeArray','nativeString','canonical','unansweredQuestion','TaskBoardNativeInspector','Date','Intl',stripTypeScriptTypes(sameSource)+'\n'+stripTypeScriptTypes(source.slice(source.indexOf('const retryDelayMs')).replaceAll('export ',''))+'\nreturn {nativeRateLimitRetryAt,failedRateLimitRetryAt,rateLimitWaitingDetail};')(nativeRecord,nativeArray,nativeString,canonical,question,Inspector,ParsedDate,IntlProbe);
 const engine={store:{async read(key,address,primary=true){return call('read',[key,address,primary],()=>{fault(key==='task-board/run'?'run':'result');return structuredClone(key==='task-board/run'?row.run:row.result);});}}};
 let outcome;try{outcome={ok:kind==='pure'?subject.nativeRateLimitRetryAt(row.turn,row.observedAt):await subject.failedRateLimitRetryAt(engine,row.task)};}catch(e){outcome={error:failure(e)};}
 row.boundaries=boundaries;row.expected=outcome;row.expectedHash=hashJson(outcome);
};
for(const row of pure)await execute(row,'pure');
for(const row of guards)await execute(row,'guard');
const waiting=['2026-01-02T03:04:05.000Z','\ud800'].map(at=>({at,text:'Agent temporarily unavailable; automatic continuation after '+at+'.'}));
process.stdout.write(JSON.stringify({pure,guards,waiting}));
