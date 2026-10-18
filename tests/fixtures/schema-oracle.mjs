import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {pointerParts,scalarTypes}=await import('../../packages/contracts/src/schema.ts');
const {bundleLocalSchema}=await import('../../packages/contracts/src/local-bundle.ts');
const rows=[];
function add(name,operation,input){
  let result;
  try {
    const value=operation==='pointer'?pointerParts(input):operation==='scalar'?[...scalarTypes(input.schema,input.pointer)]:bundleLocalSchema(input.document,input.name);
    result={ok:true,value};
  } catch(error) {result={ok:false,code:error.code,message:error.message};}
  rows.push({name:JSON.stringify(name),operation,input:JSON.stringify(input),result:JSON.stringify(result)});
}
for(const pointer of ['', '#/', 'x', '/', '//', '/~0', '/~1', '/~01', '/~10', '/~', '/~2', '/a~0b~1c', '/a/~1/~0', '/\ud800/\udc00', '/\u0000', '/😀', '/a\n', '/0/01/-1/1e2'])add(`pointer ${JSON.stringify(pointer)}`,'pointer',pointer);
const field=(schema,pointer='/v',name=JSON.stringify(schema))=>add(name,'scalar',{schema:{properties:{v:schema}},pointer});
for(const type of ['string','number','integer','boolean','null','object','array']){
  field({type}); field({type:[type,'null']}); field({type:[type,type]});
  field({type,allOf:[{description:'annotation'},true]});
  for(const other of ['string','number','integer','boolean','null','object','array']){
    for(const keyword of ['allOf','anyOf','oneOf'])field({[keyword]:[{type:[type,'null']},{type:[other,'null']}]});
  }
}
for(const literal of [null,true,false,0,-1,1.25,'','\ud800',[],{},[1]]){
  field({const:literal});field({enum:[literal]});field({enum:[literal,null]});
}
field({const:'first',enum:[1],type:'boolean'});
field({allOf:[{type:'string'},{type:'number'},{type:'string'}]});
field({type:'string',allOf:[{type:'number'}]});
field({type:'object',anyOf:[{type:'string'}],oneOf:[{type:'number'}]});
for(const pointer of ['/v/0','/v/1','/v/01','/v/-1','/v/4294967295','/v/99999999999999999999999999999999','/v/0/x','/v/','/v/1e2'])field({type:'array',items:{type:'integer',properties:{x:{type:'string'}}}},pointer);
for(const key of ['', '~', '/', '~1', '__proto__','constructor','\ud800','\udc00','😀']){
  add(`escaped ${JSON.stringify(key)}`,'scalar',{schema:{properties:Object.fromEntries([[key,{type:'string'}]])},pointer:'/'+key.replaceAll('~','~0').replaceAll('/','~1')});
}
for(const reference of ['#/$defs/x','#/$defs/a~1b','#/$defs/~01','#/$defs/\ud800','#/default/0','#/default/1','#/default/01','#/default/length','#/default/length/x','#/default/2','#/const','#/type','#/missing','#/bad~2','#','remote#/$defs/x']){
  // Refs to arrays are legal local pointers, though only bool/object final targets are schemas.
  add(`ref ${reference}`,'scalar',{schema:{$defs:{x:{type:'integer'},'a/b':{type:'string'},'~1':{type:'boolean'},'\ud800':{type:'null'}},properties:{v:{$ref:reference,type:'string'}},default:[{type:'number'},false],const:3,type:'object'},pointer:'/v'});
}
add('scalar consuming recursive','scalar',{schema:{$defs:{node:{type:'object',properties:{v:{type:'string'},next:{$ref:'#/$defs/node'}}}},$ref:'#/$defs/node'},pointer:'/next/next/v'});
add('scalar nonconsuming recursive','scalar',{schema:{$defs:{node:{$ref:'#/$defs/node'}},$ref:'#/$defs/node'},pointer:'/v'});
for(const count of [63,64,65]){
  let schema={type:'string'};
  for(let i=0;i<count;i++)schema={properties:{v:schema}};
  add(`depth ${count}`,'scalar',{schema,pointer:'/v'.repeat(count)});
}
const bundle=(document,name='Root',label=JSON.stringify(document))=>add(label,'bundle',{document,name});
bundle({$schema:'draft', $defs:{Root:{type:'object',properties:{value:{$ref:'#/$defs/a~1b'},next:{$ref:'#/$defs/Root'}},additionalProperties:false},'a/b':{type:'integer'},Unused:{type:'string'}}});
bundle({$defs:{Root:{properties:{default:{$ref:'#/$defs/Value'}},const:{$ref:'external'},default:{$ref:'external'},examples:[{$ref:'external'}]},Value:{type:'string'}}});
bundle({$defs:{Root:{properties:{x:{$ref:'#/$defs/False'}},items:{$ref:'#/$defs/True'}},False:false,True:true}});
for(const key of ['', '/', '~', '~1','~2','__proto__','constructor','\ud800','a\n','9','2']){
  bundle({$defs:Object.fromEntries([[key,{const:1}]])},key,`root ${JSON.stringify(key)}`);
  bundle({$defs:Object.fromEntries([['Root',{$ref:'#/$defs/'+key.replaceAll('~','~0').replaceAll('/','~1')}],[key,{const:1}]])},'Root',`reference ${JSON.stringify(key)}`);
}
for(const reference of ['https://example.invalid','#','#/x','#/$defs/','#/$defs/Value/properties/x','#/$defs/Value/missing','#/$defs/Value/\n','#/$defs/Value/a\nb','#/$defs/Value/a\r\n','#/$defs/Value/a\u2028','#/$defs/Value/a\u2029','#/$defs/Missing',null,7])bundle({$defs:{Root:{$ref:reference},Value:{properties:{x:{type:'string'}}}}});
bundle({$defs:{Root:{allOf:[{$ref:'#/$defs/B'},{$ref:'#/$defs/C'}]},B:{$ref:'#/$defs/D'},C:{type:'null'},D:{$ref:'#/$defs/Root'}}});
bundle({$defs:{Root:{properties:{'9':{$ref:'#/$defs/C'},'2':{$ref:'#/$defs/B'},x:{$ref:'#/$defs/D'}}},B:{},C:{},D:{}}});
bundle({$defs:{}},'Missing');
process.stdout.write(JSON.stringify(rows));
