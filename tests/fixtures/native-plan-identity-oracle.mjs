import {readFileSync,readdirSync,existsSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,next){
  if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){
    const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);
    if(existsSync(fileURLToPath(u)))return next(u.href,c);
  }
  return next(s,c);
}});
const {NativeContract}=await import('../../packages/contracts/src/native-contract.ts');
const {validateNativePlanIdentity}=await import('../../packages/sdk/src/native-plan.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const methods={threadStart:'thread/start',threadResume:'thread/resume',turnStart:'turn/start',turnInterrupt:'turn/interrupt'};
const zero='sha256:'+'0'.repeat(64), rows=[];
function capture(contract,plan){
  try{validateNativePlanIdentity(contract,plan);return {ok:true,value:null};}
  catch(error){const e=IvyError.from(error);return {ok:false,error:{code:e.code,message:e.message,outcome:e.outcome}};}
}
for(const directory of readdirSync('specs/native').filter(s=>/^codex-/.test(s))){
  const path='specs/native/'+directory+'/catalog.json';
  if(!existsSync(path))continue;
  const source=JSON.parse(readFileSync(path,'utf8')),contract=new NativeContract(source);
  const definitions=Object.fromEntries(Object.entries(methods).map(([key,method])=>[key,contract.definitionHash(method)]));
  const original={nativeVersion:contract.catalog.version,catalogSourceHash:contract.catalog.sourceHash,definitions};
  const cases=[];
  const add=(name,plan)=>cases.push({name,plan:JSON.stringify(plan),expected:capture(contract,plan)});
  add('exact identity',original);
  add('identity is not draft validation',{...original,threadStart:null,threadResume:false,turnStart:17,extra:'retained'});
  for(const invalid of [null,false,0,'',[],{},'\ud800'])add('non-plan '+JSON.stringify(invalid),invalid);
  for(const field of ['nativeVersion','catalogSourceHash']){
    const missing={...original};delete missing[field];add('missing '+field,missing);
    for(const value of [null,false,0,'','other',zero,'\ud800',{},[]])add('invalid '+field+' '+JSON.stringify(value),{...original,[field]:value});
  }
  const missingDefinitions={...original};delete missingDefinitions.definitions;add('missing definitions',missingDefinitions);
  for(const value of [null,false,0,'',[],{}])add('definitions '+JSON.stringify(value),{...original,definitions:value});
  for(const key of Object.keys(methods)){
    const missing={...definitions};delete missing[key];add('missing definition '+key,{...original,definitions:missing});
    for(const value of [null,false,0,'',zero,'\ud800',{},[]])add('definition '+key+' '+JSON.stringify(value),{...original,definitions:{...definitions,[key]:value}});
  }
  add('catalog error precedes missing definitions',{nativeVersion:'other',catalogSourceHash:zero});
  const alteredDefinitions={...definitions,threadStart:zero,turnInterrupt:zero};
  add('first definition precedes later mismatch',{...original,definitions:alteredDefinitions});
  rows.push({path,cases});
  // Missing methods must preserve the original failure ordering, including the
  // left-hand property access before definitionHash is evaluated.
  for(const removedMethod of Object.values(methods)){
    const changed={...source,clientRequests:source.clientRequests.filter(m=>m.method!==removedMethod)};
    const selected=new NativeContract(changed);
    const available=Object.fromEntries(Object.entries(methods).map(([key,method])=>[key,method===removedMethod?zero:selected.definitionHash(method)]));
    const plan={...original,definitions:available};
    rows.push({path,removedMethod,cases:[
      {name:'missing selected method',plan:JSON.stringify(plan),expected:capture(selected,plan)},
      {name:'null definitions before missing method',plan:JSON.stringify({...plan,definitions:null}),expected:capture(selected,{...plan,definitions:null})},
      {name:'catalog mismatch before missing method',plan:JSON.stringify({...plan,nativeVersion:'other'}),expected:capture(selected,{...plan,nativeVersion:'other'})},
      {name:'earlier wrong definition',plan:JSON.stringify({...plan,definitions:{...available,threadStart:zero}}),expected:capture(selected,{...plan,definitions:{...available,threadStart:zero}})},
    ]});
  }
}
process.stdout.write(JSON.stringify(rows));
