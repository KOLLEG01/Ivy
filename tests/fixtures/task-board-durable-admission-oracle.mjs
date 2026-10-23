// Reuse qualified original cases; capture only new receipt refusal risks from
// the supplied actual canonical validator module and its complete registry.
import fs from 'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {parseArgs} from 'node:util';
import {registerHooks} from 'node:module';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
const {values}=parseArgs({options:Object.fromEntries(['guard-module','runtime-root','baseline','qualified','requests','seed','restart','output','proof'].map(k=>[k,{type:'string'}]))});
for(const key of Object.keys(values))assert.ok(values[key],key);
const read=key=>JSON.parse(fs.readFileSync(values[key]));
const hash=b=>'sha256:'+createHash('sha256').update(b).digest('hex');
const qualified=read('qualified'),baseline=read('baseline'),requests=read('requests'),seed=read('seed'),restart=read('restart');
assert.equal(hash(fs.readFileSync(values['guard-module'])),qualified.stage.moduleEmittedHash);
const moduleUrl=pathToFileURL(resolve(values['guard-module'])).href,runtime=resolve(values['runtime-root']),resolution=[];
registerHooks({resolve(specifier,context,next){
 if(context.parentURL===moduleUrl){
  const target=specifier==='ajv/dist/2020.js'?resolve(runtime,'node_modules/ajv/dist/2020.js'):
    ['./canonical.js','./errors.js','./limits.js'].includes(specifier)?resolve(runtime,'dist/packages/contracts/src',specifier):null;
  if(target){const bytes=fs.readFileSync(target);resolution.push({specifier,byteLength:bytes.length,contentHash:hash(bytes)});return next(pathToFileURL(target).href,context);}
 }
 return next(specifier,context);
}});
const {validateTaskBoard}=await import(moduleUrl);
const cases=baseline.cases.map(c=>{const r=qualified.results.find(r=>r.valueHash===c.valueHash&&r.definition===c.definition);assert.ok(r);assert.equal(r.execution,'completed');return{name:c.name,definition:c.definition,value:c.value,expected:{valid:r.finding.valid,error:r.error},reused:true,valueHash:c.valueHash};});
for(const c of requests.cases)cases.push({name:'request '+c.name,definition:'DurableRequest',value:c.value,expected:{valid:c.after.DurableRequest.valid,error:c.after.DurableRequest.error??null},reused:true,valueHash:c.requestHash});
const receipt=structuredClone(baseline.cases.find(c=>c.definition==='OperationReceipt'&&c.value.phase==='succeeded').value);
const changes=[
 ['missing receipt identity',v=>delete v.operationId],['unknown receipt field',v=>v.unrecognized=true],
 ['missing receipt marker',v=>delete v.receipt],['false receipt marker',v=>v.receipt=false],
 ['invalid receipt phase',v=>v.phase='completed'],['malformed submitted hash',v=>v.submittedHash='invalid'],
 ['invalid nested outcome identity',v=>v.outcome.operationId=0],['missing caller identity',v=>delete v.callerPrincipalId],
 ['malformed receipt pin',v=>v.handoffRun.revision=0],['missing retained request hash',v=>delete v.requestHash],
 ['invalid completion error',v=>v.error={code:'error'}],['null original operation identity',v=>v.operationId=null],
];
for(const [name,change]of changes){const value=structuredClone(receipt);change(value);let error=null;try{validateTaskBoard('OperationReceipt',value);}catch(e){error={code:e.code,message:e.message,outcome:e.outcome,details:e.details??null};}assert.ok(error,name);cases.push({name,definition:'OperationReceipt',value,expected:{valid:!error,error},reused:false});}
const pending=seed.rows.find(r=>r.label==='uncertain-prepared'),entry=seed.uncertain.entry;
const snapshots=[...seed.snapshots,...restart.snapshots];
const snapshot=label=>{const found=snapshots.find(s=>s.label===label);assert.ok(found,label);return found;};
const payloadRow=seed.logicalTables.revisions.find(r=>r.object_id===entry.payload.object.objectId||r.id===entry.payload.object.objectId);
assert.ok(payloadRow,'Retained prepared payload');
const payload=JSON.parse(payloadRow.value_json);
const protocol={
 accepted:seed.rows.map(row=>({label:row.label,pin:row.pin,value:snapshot(row.label+'-accepted').document.value})),
 pending:{row:pending,entry,effect:seed.uncertain.effect,payload,
   accepted:snapshot('uncertain-prepared-accepted').document.value,
   prepared:snapshot('uncertain-prepared').document.value,
   publishing:snapshot('resumed-uncertain-local-create').document.value,
   completed:snapshot('resumed-uncertain-completed').document.value},
 refusals:seed.errors,
 epoch:seed.backend.epoch,
};
const inputPins=Object.fromEntries(['baseline','qualified','requests','seed','restart'].map(key=>{const b=fs.readFileSync(values[key]);return[key,{byteLength:b.length,contentHash:hash(b)}];}));
const fixture={schemaVersion:1,cases,protocol,originalInputs:inputPins,guardModule:{contentHash:qualified.stage.moduleEmittedHash,sourceHash:qualified.stage.moduleSourceHash,documents:qualified.stage.documents.map(({name,byteLength,contentHash,schemaId})=>({name,byteLength,contentHash,schemaId}))}};
fs.writeFileSync(values.output,JSON.stringify(fixture,null,2)+'\n');
const count={reused:cases.filter(c=>c.reused).length,new:cases.filter(c=>!c.reused).length,newRefused:cases.filter(c=>!c.reused&&!c.expected.valid).length};
fs.writeFileSync(values.proof,JSON.stringify({at:new Date().toISOString(),counts:count,total:cases.length,fixture:{byteLength:fs.statSync(values.output).size,contentHash:hash(fs.readFileSync(values.output))},inputPins,guardModule:fixture.guardModule,resolution,originalPassingCasesReexecuted:0,originalReaderReexecuted:false,scope:'New receipt mutations only use the unchanged approved canonical module. Original54 complete-record and six request observations plus original protocol rows/errors are reused verbatim; no original guard/reader/build was repeated.'},null,2)+'\n');
console.log(JSON.stringify({cases:cases.length,counts:count,nativeChecksExecuted:false}));
