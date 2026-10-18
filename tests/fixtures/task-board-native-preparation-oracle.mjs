import {existsSync,readFileSync,readdirSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {checkedNativeContract}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {saveNativePlan,readNativePlan,validateNativeInvocation}=await import('../../packages/sdk/src/native-plan.ts');
const source=readFileSync('services/task-board/src/runtime/native-intent.ts','utf8');
const code=source.slice(source.indexOf('export type NativeSlot'),source.indexOf('export async function retainNativeRequest')).replaceAll('export ','');
const zero='sha256:'+'0'.repeat(64),pin={objectId:'00000000-0000-4000-8000-000000000100',revision:1};
const primary={namespace:'codex',kind:'thread',serviceNodeId:'native-agent',nativeId:'thread'};
const initial={taskKey:'TASK-0123',primaryResourceRef:primary,marker:'pinned original task'};
const current={...initial,taskKey:'TASK-0124',marker:'current task',primaryResourceRef:{...primary,nativeId:'new-current-thread'}};
const origin={kind:'steer',commentIds:['comment'],workRevision:1,peersHash:zero,predecessor:null};
const reattach={kind:'reattach',predecessor:{objectId:'00000000-0000-4000-8000-000000000300',revision:1},preparedEpoch:'epoch'};
const groups=[];
for(const directory of readdirSync('specs/native').filter(n=>/^codex-/.test(n))) {
  const path='specs/native/'+directory+'/catalog.json';if(!existsSync(path))continue;
  const c=JSON.parse(readFileSync(path,'utf8')),catalog=checkedNativeContract(c.version,{sourceHash:c.sourceHash});
  const client={request(){throw Error('Unexpected native/Hive call while preparing the original fixture');}};
  const draft={nativeVersion:c.version,catalogSourceHash:c.sourceHash,definitions:Object.fromEntries(Object.entries({threadStart:'thread/start',threadResume:'thread/resume',turnStart:'turn/start',turnInterrupt:'turn/interrupt'}).map(([k,m])=>[k,hashJson(catalog.definitions.get(m))])),threadStart:{cwd:'/workspace'},threadResume:{cwd:'/workspace'},turnStart:{input:[{type:'text',text:'retained input',text_elements:[]}]}};
  const retained=await saveNativePlan(client,catalog.contract,{draft,kind:'task-board',parentId:'root',mutationId:'plan'});
  const plan=await readNativePlan(client,catalog.contract,{plan:retained,kind:'task-board',parentId:'root'});
  const run={taskId:pin.objectId,plan,primaryResourceRef:primary,turnId:'turn',nativeEpoch:'epoch',cancellation:null,target:{serviceNodeId:'native-agent'},owner:{source:'user',serviceNodeId:'task-board-owner'},originalRequest:{action:'continue',expectedRevision:1,coordinationOnly:false}};
  const rows=[],add=(name,input={})=>rows.push({name,input:{slot:'thread',...input}});
  add('resume uses exact original task');add('initial default explicit',{origin:{kind:'initial'}});
  add('start without original primary',{initial:{primaryResourceRef:null}});
  add('initial ignores newer Run primary',{run:{primaryResourceRef:{...primary,nativeId:'run-thread'}}});
  add('reattach has no task read or prompt',{origin:reattach,readFailure:'fixture_failed',promptFailure:true});
  add('reattach uses current Run primary',{origin:reattach,run:{primaryResourceRef:{...primary,nativeId:'reattached'}}});
  add('turn appends retained input',{slot:'turn'});
  for(const source of ['scheduler','worker'])add('turn owner '+source,{slot:'turn',run:{owner:{...run.owner,source}}});
  add('steer uses current task',{slot:'steer',origin});
  add('interrupt exact known turn',{slot:'interrupt',run:{cancellation:{reason:'stop'}}});
  add('interrupt does not require native epoch',{slot:'interrupt',run:{nativeEpoch:null,cancellation:{reason:'stop'}}});
  add('initial steer slot preserves interrupt selection',{slot:'steer',run:{cancellation:{reason:'stop'}}});
  for(const [slot,o] of [['turn',undefined],['steer',origin]]) {
    for(const flag of [true,false,null,'truthy',0])add(slot+' coordination '+JSON.stringify(flag),{slot,origin:o,run:{originalRequest:{...run.originalRequest,coordinationOnly:flag}}});
    add(slot+' omitted coordination',{slot,origin:o,run:{originalRequest:{action:'continue',expectedRevision:1}}});
    add(slot+' non-continue ignores coordination',{slot,origin:o,run:{originalRequest:{action:'start',expectedRevision:1,coordinationOnly:true}}});
    add(slot+' prompt fails before catalog',{slot,origin:o,promptFailure:true,plan:{catalogSourceHash:zero}});
    add(slot+' UTF16 owner and prompt',{slot,origin:o,run:{owner:{...run.owner,serviceNodeId:'owner\udfff'}},prompt:'prompt\n\ud800'});
    add(slot+' prompt preflight limit',{slot,origin:o,large:true});
  }
  for(const patch of [{primaryResourceRef:null},{nativeEpoch:null}])add('turn guard '+JSON.stringify(patch),{slot:'turn',run:patch,readFailure:'fixture_failed',promptFailure:true});
  for(const patch of [{primaryResourceRef:null},{nativeEpoch:null},{turnId:null},{cancellation:{}}])add('steer guard '+JSON.stringify(patch),{slot:'steer',origin,run:patch,readFailure:'fixture_failed'});
  for(const slot of ['thread','turn','interrupt'])add('steer wrong slot '+slot,{slot,origin,readFailure:'fixture_failed'});
  for(const slot of ['turn','steer','interrupt'])add('reattach wrong slot '+slot,{slot,origin:reattach,readFailure:'fixture_failed'});
  for(const patch of [null,{...primary,namespace:'other'},{...primary,kind:'other'},{...primary,serviceNodeId:'other'}]) {
    add('reattach primary '+JSON.stringify(patch),{origin:reattach,run:{primaryResourceRef:patch}});
    add('initial primary '+JSON.stringify(patch),{initial:{primaryResourceRef:patch}});
  }
  for(const patch of [{primaryResourceRef:null,cancellation:{}},{turnId:null,cancellation:{}},{cancellation:null}])add('interrupt guard '+JSON.stringify(patch),{slot:'interrupt',run:patch,readFailure:'fixture_failed'});
  for(const value of [null,{}, {kind:'initial',extra:true},{kind:'unknown'},{kind:'reattach'},{...origin,workRevision:-1}])add('origin admission '+JSON.stringify(value),{origin:value,readFailure:'fixture_failed',plan:{catalogSourceHash:zero}});
  for(const slot of ['thread','turn','steer'])for(const failure of ['not_found','fixture_failed'])add(slot+' task read '+failure,{slot,origin:slot==='steer'?origin:undefined,readFailure:failure,promptFailure:true});
  for(const nativeVersion of ['unsupported',''])add('selected native version '+nativeVersion,{plan:{nativeVersion}});
  add('selected source mismatch after read',{plan:{catalogSourceHash:zero}});
  for(const [slot,key,extra] of [['thread','threadResume',{}],['thread','threadStart',{initial:{primaryResourceRef:null}}],['turn','turnStart',{}],['interrupt','turnInterrupt',{run:{cancellation:{}}}]])add('retained selected definition '+key,{slot,...extra,plan:{definitions:{...plan.definitions,[key]:zero}}});
  add('steer computes definition instead of reading saved hashes',{slot:'steer',origin,plan:{definitions:null}});
  add('resume overrides saved thread and pagination',{plan:{threadResume:{...plan.threadResume,threadId:'wrong',excludeTurns:false}}});
  add('turn overrides thread and preserves native options',{slot:'turn',plan:{turnStart:{...plan.turnStart,threadId:'wrong',model:'retained-model'}}});
  for(const input of [null,'not-an-array',{}])add('non-array retained input '+JSON.stringify(input),{slot:'turn',plan:{turnStart:{input}}});
  for(const threadResume of [null,'A\ud800',[1,2]])add('JSON parameter spread '+JSON.stringify(threadResume),{plan:{threadResume}});
  add('invocation refuses unknown resume parameter',{plan:{threadResume:{unexpected:true}}});
  add('thread identity preserves UTF16',{initial:{primaryResourceRef:{...primary,nativeId:'thread\ud800'}}});
  for(const row of rows) {
    const i=row.input,active={...run,...i.run,plan:{...plan,...i.plan}},trace=[];
    const store={async read(key,address,upgrade=true) {
      trace.push(['read',key,address,upgrade]);assert.equal(key,'task-board/task');
      if(i.readFailure)throw new IvyError(i.readFailure,'Fixture task unavailable.');
      assert.deepEqual(address,typeof address==='string'?pin.objectId:pin);
      return {pin:{objectId:pin.objectId,revision:typeof address==='string'?2:1},value:typeof address==='string'?{...current,...i.current}:{...initial,...i.initial}};
    }};
    const prompt=(kind,task,owner,flag)=>{
      trace.push(['prompt',kind,task,owner,flag===undefined?{undefined:true}:flag]);
      if(i.promptFailure)throw new IvyError('fixture_failed','Fixture prompt unavailable.');
      return i.large?'x'.repeat(1048576):i.prompt??'rendered\n\ud800';
    };
    const validate=(name,value)=>{trace.push(['validate',name]);validateTaskBoard(name,value);};
    const intendedCall=new Function('taskPrompt','taskUpdatePrompt','hashJson','requireThat','validateTaskBoard','checkedNativeContract','validateNativeInvocation',stripTypeScriptTypes(code)+'\nreturn intendedCall;')((...args)=>prompt('task',...args),(...args)=>prompt('update',...args),hashJson,requireThat,validate,checkedNativeContract,validateNativeInvocation);
    let result;try{result={ok:await intendedCall(store,active,i.slot,i.origin)};}catch(error){if(!(error instanceof IvyError))throw new Error('Original preparation fixture '+row.name,{cause:error});result={error:{code:error.code,message:error.message}};}
    if(['resume uses exact original task','start without original primary','reattach has no task read or prompt','turn appends retained input','turn owner scheduler','steer uses current task','interrupt exact known turn','initial steer slot preserves interrupt selection','steer computes definition instead of reading saved hashes'].includes(row.name))assert.ok(result.ok,row.name+JSON.stringify(result));
    row.expectedHash=hashJson({result,trace});
    row.reads=trace.filter(e=>e[0]==='read').map(e=>typeof e[2]==='string'?{objectId:e[2]}:e[2]);
  }
  groups.push({path,base:{run,initial,current,pin},rows});
}
assert.equal(groups.length,4);assert.ok(groups.every(g=>g.rows.length>=80));
process.stdout.write(JSON.stringify(groups));
