import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {queryDocument}=await import('../../packages/sdk/src/client.ts');
const rows=[];
function add(input,fn){
  let result;
  try{result={ok:true,value:fn()};}
  catch(e){result={ok:false,error:{code:e.code??null,message:e.message,outcome:e.outcome??null}};}
  rows.push({input:JSON.stringify(input),result:JSON.stringify(result)});
}
for(const objectId of ['object','\ud800']){
  const row={objectId,revision:7,contractVersion:'1.2.3',values:{name:'value'},document:{object:{id:objectId,extra:'preserved'},revision:{objectId,revision:7,contractVersion:'1.2.3'},content:{encoding:'utf8',value:'\udfff'},extra:{untouched:true}}};
  function query(input){add({kind:'query',value:input},()=>{const result=queryDocument(input);assert.equal(result,input.document);return result;});}
  query(row);
  const missing=structuredClone(row);delete missing.document;query(missing);
  query({...row,document:null});
  for(const [section,key,value] of [['object','id','other'],['revision','objectId','other'],['revision','revision',8],['revision','contractVersion','1.2.4']]){
    const changed=structuredClone(row);changed.document[section][key]=value;query(changed);
  }
}
const values=new Set([0,-0,0.9,-0.9,1.9,-1.9,999.9,-999.9,NaN,Infinity,-Infinity,8640000000000000,-8640000000000000,8640000000000001,-8640000000000001]);
const around=t=>{for(const delta of [-86400000,-1,0,1,86400000])values.add(t+delta);};
// Every leap-year state in a Gregorian era, including the non-leap centuries.
for(let year=1800;year<2200;year++)around(Date.parse(`${year}-03-01T00:00:00.000Z`));
// Month/year/extended-year transitions on either side of year zero and near TimeClip.
for(const year of [-271821,-100000,-10000,-400,-100,-4,-1,0,1,4,99,100,400,9999,10000,100000,275760]){
  const y=year>=0&&year<=9999?String(year).padStart(4,'0'):(year<0?'-':'+')+String(Math.abs(year)).padStart(6,'0');
  for(let month=1;month<=12;month++)around(Date.parse(`${y}-${String(month).padStart(2,'0')}-01T00:00:00.000Z`));
}
// Deterministic coverage throughout the full JS range, with no wall-clock dependency.
let state=0x12345678;
for(let i=0;i<2048;i++){state=(Math.imul(state,1664525)+1013904223)>>>0;values.add(Math.trunc((state/0xffffffff*2-1)*8640000000000000));}
for(const value of values)add({kind:'date',value:Number.isFinite(value)?value:String(value)},()=>new Date(value).toISOString());
process.stdout.write(JSON.stringify(rows));
