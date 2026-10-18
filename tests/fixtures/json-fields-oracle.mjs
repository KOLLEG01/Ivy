import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {canonical}=await import('../../packages/contracts/src/canonical-json.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const rows=[];
function add(input){
  let fields=input.fields;
  if(input.generated==='nodes')fields=[{key:'a',value:Array(input.count).fill(null)},{key:'b'}];
  if(input.generated==='depth'){
    let value=null;for(let i=0;i<input.depth;i++)value=[value];
    fields=[{key:'a',value},{key:'b'}];
  }
  if(input.generated==='number')fields=[{key:'a',value:({nan:NaN,infinity:Infinity,negative:-Infinity})[input.number]},{key:'b'}];
  let result;
  try{result={ok:true,value:canonical(Object.fromEntries(fields.map(field=>[field.key,field.value])),input.maximumBytes)};}
  catch(error){result={ok:false,error:IvyError.from(error).toWire()};}
  rows.push({input:JSON.stringify(input),result:JSON.stringify(result)});
}
const fieldSets=[
  [],[{key:'a'}],[{key:'a',value:null}],[{key:'a',value:1},{key:'b'}],
  [{key:'b'},{key:'a',value:9007199254740992}],
  [{key:'a'},{key:'b',value:9007199254740992}],
  [{key:'a',value:1},{key:'a'}],[{key:'a'},{key:'a',value:1}],
  [{key:'__proto__',value:{retained:true}},{key:'constructor',value:1},{key:'toString',value:'text'}],
  [{key:'\ud800',value:'\udfff'},{key:'😀',value:'é'},{key:'2',value:2},{key:'10',value:10}],
];
for(const value of [null,false,0,'',true,'a','\ud800','é',[],[1,null],{a:1},{a:9007199254740992}])fieldSets.push([{key:'before',value},{key:'missing'}]);
for(const fields of fieldSets)for(const maximumBytes of [0,1,2,3,4,5,6,8,10,16,24,32,64,128,1024])add({fields,maximumBytes});
for(const number of ['nan','infinity','negative'])for(const maximumBytes of [0,3,4,100])add({generated:'number',number,maximumBytes});
for(const count of [399996,399997,399998,399999,400000])add({generated:'nodes',count,maximumBytes:33554432});
for(const depth of [126,127,128,129])add({generated:'depth',depth,maximumBytes:33554432});
process.stdout.write(JSON.stringify(rows));
