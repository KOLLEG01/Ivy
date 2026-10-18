import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
import assert from 'node:assert/strict';
registerHooks({resolve(specifier,context,next){
  if(specifier.endsWith('.js')&&specifier.startsWith('.')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }return next(specifier,context);
}});
const {ReceiptArchive,hasUnresolvedOutcome}=await import('../../packages/sdk/src/receipt-archive.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const [mode,path]=process.argv.slice(2);
if(mode==='coercion'){
 const capture=fn=>{try{return {ok:true,value:fn()};}catch(e){return {ok:false,error:IvyError.from(e).toWire()};}};
 const values=[null,false,0,'unknown',{},[],{phase:'unknown'},{code:'native_epoch_lost'},{errorCode:['prefix','outcome_unknown']},{phase:[[null]]},{phase:{ordinary:{toString:null}}}];
 for(const key of ['phase','state','ownerPhase','outcome','execution','error','errorCode','code','ordinary','0','01','4294967294','4294967295','\ud800']){
  for(const part of [null,false,0,'done','unknown','outcome_unknown','needs_attention','native_connection_lost','prefix_outcome_unknown_suffix',{},[],['unknown'],[['unknown']],{toString:null},[{toString:0}],[[{toString:'x'}]],{nested:{toString:null}},'\ud800outcome_unknown'])values.push({[key]:part});
 }
 const failure={phase:{toString:null}},unresolved={outcome:'unknown'};
 for(const a of [failure,unresolved,{nested:failure},{nested:unresolved}])for(const b of [failure,unresolved,{nested:failure},{nested:unresolved}]){
  values.push([a,b],{first:a,second:b},{'1':a,'0':b},JSON.parse('{"1":'+JSON.stringify(a)+',"0":'+JSON.stringify(b)+'}'),{'01':a,'1':b});
 }
 const inputs=values.map(value=>JSON.stringify(value));
 for(const [a,b] of [[failure,unresolved],[unresolved,failure]])for(const [first,second] of [['2','1'],['4294967295','4294967294'],['01','0']])inputs.push('{'+JSON.stringify(first)+':'+JSON.stringify(a)+','+JSON.stringify(second)+':'+JSON.stringify(b)+'}');
 const rows=inputs.map(text=>{
  const value=JSON.parse(text),detection=capture(()=>hasUnresolvedOutcome(value)),releases=[];
  for(const transaction of [false,true]){
   const db=new DatabaseSync(':memory:'),archive=new ReceiptArchive(db,{count:0,bytes:0});
   archive.retain('op','case',text);if(transaction)db.exec('BEGIN');
   const result=capture(()=>{archive.release('op','case');return null;});
   const snapshot={result,records:archive.status().records,history:db.prepare('SELECT COUNT(*) AS n FROM receipt_archive_history').get().n,retired:db.prepare('SELECT COUNT(*) AS n FROM receipt_archive_retired').get().n,autocommit:!db.isTransaction};
   releases.push(JSON.stringify(snapshot));if(transaction)db.exec('ROLLBACK');db.close();
  }
  return {value:text,detection:JSON.stringify(detection),releases};
 });
 process.stdout.write(JSON.stringify(rows));
}else{
const db=new DatabaseSync(path);const archive=new ReceiptArchive(db,{count:1,bytes:1000});
if(mode==='seed'){
  archive.retain('operation','js','{"value":"😀","phase":"done"}');
  archive.retain('operation','unknown','{"owner":{"outcome":"unknown"}}');
}else{
  assert.throws(()=>archive.read('operation','js'),{code:'operation_expired'});
  assert.equal(archive.read('operation','rust'),'original Rust receipt');
  assert.throws(()=>archive.release('operation','unknown'),{code:'receipt_archive_unresolved'});
  assert.equal(archive.status().records,2);
}db.close();
}
