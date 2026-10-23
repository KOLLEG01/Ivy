import {readFileSync} from 'node:fs';
import ts from 'typescript';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
const source=readFileSync('services/agent-manager/src/main.ts','utf8'),file=ts.createSourceFile('main.ts',source,ts.ScriptTarget.Latest,true);let expression;
function visit(node){if(ts.isVariableDeclaration(node)&&node.name.getText(file)==='track')expression=node.initializer.getText(file);ts.forEachChild(node,visit);}visit(file);if(!expression)throw Error('Original track function missing');
const start=(name,extra={})=>({action:'start',name,...extra}),release=name=>({action:'release',name}),wait=name=>({action:'wait',name}),drop=name=>({action:'drop',name}),clone=(name,target)=>({action:'clone',name,target}),snapshot=name=>({action:'snapshot',name}),waitSnapshot=name=>({action:'waitSnapshot',name});
const cases=[
  {name:'result-and-deletion-before-wait',steps:[start('a',{value:{text:'\ud800',ok:true}}),release('a'),wait('a')]},
  {name:'rejection-and-deletion-before-wait',steps:[start('a',{fail:true}),release('a'),wait('a')]},
  {name:'dropped-waiter-keeps-operation-owned',steps:[start('a'),drop('a'),release('a'),{action:'drain'}]},
  {name:'duplicate-waiter-observes-original-result',steps:[start('a',{value:[null,1,'value']}),clone('a','alias'),drop('a'),release('a'),wait('alias'),{action:'drain'}]},
  {name:'drain-settles-success-and-failure',steps:[start('a'),start('b',{fail:true}),release('b'),release('a'),{action:'drain'}]},
  {name:'snapshot-excludes-later-work',steps:[start('a'),snapshot('before'),start('b'),release('a'),waitSnapshot('before'),release('b'),{action:'drain'}]},
  {name:'empty-snapshot-excludes-new-work',steps:[snapshot('empty'),start('a'),waitSnapshot('empty'),release('a'),{action:'drain'}]},
  {name:'multiple-callers-keep-independent-work',steps:[start('a'),start('b'),clone('a','alias'),release('a'),wait('alias'),drop('a'),release('b'),wait('b'),{action:'drain'}]},
];
for(const row of cases){
  const work=new Set(),handles=new Map(),gates=new Map(),snapshots=new Map(),completed=[];
  const track=Function('work',ts.transpile('const track='+expression+';',{target:ts.ScriptTarget.ES2022})+'return track;')(work),trace=[];
  for(const step of row.steps){let outcome=null;
    if(step.action==='start'){
      let finish;const gate=new Promise(resolve=>{finish=resolve;});gates.set(step.name,finish);
      handles.set(step.name,track((async()=>{await gate;completed.push(step.name);if(step.fail)throw new IvyError('fixture_work_failed','Fixture work failed.');return step.value??null;})()));
    }else if(step.action==='release'){gates.get(step.name)();continue;}
    else if(step.action==='clone')handles.set(step.target,handles.get(step.name));
    else if(step.action==='drop')handles.delete(step.name);
    else if(step.action==='snapshot')snapshots.set(step.name,Promise.allSettled([...work]));
    else if(step.action==='waitSnapshot')await snapshots.get(step.name);
    else if(step.action==='drain')await Promise.allSettled([...work]);
    else if(step.action==='wait'){try{outcome={result:await handles.get(step.name)};}catch(error){outcome={error:IvyError.from(error).toWire()};}}
    trace.push({action:step.action,pending:work.size,...(outcome??{})});
  }
  row.expected={trace,completed:completed.sort()};
}
console.log(JSON.stringify({cases}));
