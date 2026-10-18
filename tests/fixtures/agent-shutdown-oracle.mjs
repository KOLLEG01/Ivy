import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {isDeepStrictEqual} from 'node:util';
import ts from 'typescript';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
const source=readFileSync('services/agent-manager/src/main.ts','utf8'),file=ts.createSourceFile('main.ts',source,ts.ScriptTarget.Latest,true);let expression;
function visit(node){if(ts.isVariableDeclaration(node)&&node.name.getText(file)==='close'&&node.initializer&&ts.isArrowFunction(node.initializer))expression=node.initializer.getText(file);ts.forEachChild(node,visit);}visit(file);assert.ok(expression);
const phases=['native','work-snapshot','service','instructions','mcp','skills','locations','state','health-work','health','journal'];
const failures=name=>new IvyError(name==='native'?'outcome_unknown':'fixture_'+name+'_failed','Original '+name+' cleanup failure\ud800',name==='native'?'unknown':'not_executed',{phase:name,retained:'\udfff'});
const cases=[{name:'success',fail:[]},...['native','service','instructions','mcp','skills','locations','state','health','journal'].map(name=>({name:name+'-failure',fail:[name]})),
 {name:'pending-health-rejection-remains-ignored',fail:['health-work']},
 {name:'native-unknown-wins-over-journal',fail:['native','journal']},
 {name:'native-unknown-wins-over-all-later-errors',fail:['native','service','instructions','mcp','skills','locations','state','health','journal']},
 {name:'service-error-wins-over-manager-error',fail:['service','skills']},
 {name:'immediate-manager-errors-preserve-first-observation',fail:['instructions','skills']},
 {name:'requested-success-code-is-retained',fail:[],requestedCode:'requested\ud800'},
 {name:'partial-startup-has-no-optional-owners',fail:[],missing:['native','service','instructions','mcp','skills','locations']},
];
for(const row of cases){
 const calls=[],healthCodes=[];let closedCode;
 const sync=name=>()=>{calls.push(name);if(row.fail.includes(name))throw failures(name);};
 const asyncStep=name=>async()=>sync(name)();
 const bindings={native:{close:asyncStep('native')},service:{stop:asyncStep('service')},instructions:{close:asyncStep('instructions')},mcp:{close:asyncStep('mcp')},skills:{close:asyncStep('skills')},locations:{close:sync('locations')},state:{close:sync('state')},
  health:{write:async(_ready,message)=>{healthCodes.push(message);return asyncStep('health')();}},journal:{close:sync('journal')},
  work:{[Symbol.iterator](){calls.push('work-snapshot');return [Promise.resolve()].values();}},
  healthWork:{catch(onRejected){calls.push('health-work');return (row.fail.includes('health-work')?Promise.reject(failures('health-work')):Promise.resolve()).catch(onRejected);}},
  finishClosed:code=>{closedCode=code;calls.push('closed');},clearInterval:sync('timer')};
 for(const name of row.missing??[])bindings[name]=null;
 const close=Function(...Object.keys(bindings),'let stopWork=null,closing=false,timer=1;'+ts.transpile('const close='+expression+';',{target:ts.ScriptTarget.ES2022})+'return close;')(...Object.values(bindings));
 const requested=row.requestedCode??'agent_stopped',first=close(requested),samePromise=first===close('different');let result;
 try{await first;result={result:null};}catch(error){result={error:IvyError.from(error).toWire()};}
 row.original={calls,healthCodes,closedCode,samePromise,...result};
 // Explicit regression expectations for the parent-approved AM01 exception,
 // not a claim that the original source performed independent cleanup.
 const attempted=phases.filter(name=>!(row.missing??[]).includes(name));
 const failure=attempted.find(name=>name!=='journal'&&name!=='health-work'&&row.fail.includes(name));
 const journalOnly=!failure&&row.fail.includes('journal');
 const beforeHealth=attempted.find(name=>!['health','health-work','journal'].includes(name)&&row.fail.includes(name));
 row.approved={calls:['timer',...attempted,'closed'],samePromise:true,closedCode:failure?'agent_shutdown_failed':requested,
  healthCodes:['AgentManager stopped: '+(beforeHealth?'agent_shutdown_failed':requested)+'.'],
  ...(failure||journalOnly?{error:failures(failure??'journal').toWire()}:{result:null})};
 row.approvedException=!isDeepStrictEqual(row.original,row.approved);
}
console.log(JSON.stringify({sourceHash:'sha256:'+createHash('sha256').update(source).digest('hex'),approvalScope:'AM01 only: attempt existing phases once; all managers settle; first cleanup error retained; journal-only contract unchanged',cases}));
