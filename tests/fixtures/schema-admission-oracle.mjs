import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {admitSchema}=await import('../../packages/contracts/src/schema.ts');
const rows=[];
function add(name,schema){let result;try{admitSchema(schema);result={ok:true};}catch(error){result={ok:false,code:error.code,message:error.message};}rows.push({name:JSON.stringify(name),schema:JSON.stringify(schema),result:JSON.stringify(result)});}
for(const schema of [true,false,null,[],1,'x',{},{$id:1},{type:'unknown'},{required:3},{format:null},{pattern:3},{pattern:'(?=a)'},{pattern:'\\p{Cs}'},{pattern:'a'.repeat(1024)},{pattern:'a'.repeat(1025)}])add('root '+JSON.stringify(schema),schema);
for(const keyword of ['$ref','pattern','anyOf','oneOf','allOf','if','then','else','not','properties','$defs','items','additionalProperties'])for(const value of [null,true,false,{},[],[true],{},'x',1])add(keyword+' '+JSON.stringify(value),{[keyword]:value});
for(const keyword of ['unknown','patternProperties','dependentRequired','1','\ud800','x'.repeat(79)+'😀','constructor','__proto__'])add('keyword '+JSON.stringify(keyword),Object.fromEntries([[keyword,true]]));
add('numeric key precedence',Object.fromEntries([['unknown',true],['9',true],['2',true]]));
for(const keyword of ['properties','$defs']){
  for(const size of [1024,1025])add(keyword+' count '+size,{[keyword]:Object.fromEntries(Array.from({length:size},(_,i)=>['x'+i,true]))});
  add(keyword+' proto',{[keyword]:JSON.parse('{"__proto__":true}')});
}
for(const keyword of ['anyOf','oneOf','allOf'])for(const size of [0,1,32,33])add(keyword+' count '+size,{[keyword]:Array(size).fill(true)});
for(const depth of [63,64,65,128,129]){let schema=true;for(let i=0;i<depth;i++)schema={items:schema};add('depth '+depth,schema);}
const children=count=>Object.fromEntries(Array.from({length:count},(_,i)=>['p'+i,true]));
for(const last of [254,255])add('visited '+(7938+last),{allOf:Array.from({length:32},(_,i)=>({properties:children(i===31?last:255)}))});
for(const reference of ['#','external','#/missing','#/bad~2','#/properties/value','#/$defs/node','#/default/0','#/default/00','#/default/length','#/default/length/x','#/default/1','#/default/2','#/$defs/a~1b','#/$defs/~01','#/$defs/\ud800'])add('ref '+reference,{$ref:reference,$defs:{node:true,'a/b':{type:'string'},'~1':{type:'number'},'\ud800':false},properties:{value:{type:'integer'}},default:[{type:'string'},1,[]]});
add('consuming property',{$ref:'#/$defs/node',$defs:{node:{properties:{next:{$ref:'#/$defs/node'}}}}});
add('consuming items',{$ref:'#/$defs/node',$defs:{node:{items:{$ref:'#/$defs/node'}}}});
add('consuming additional',{$ref:'#/$defs/node',$defs:{node:{additionalProperties:{$ref:'#/$defs/node'}}}});
add('nonconsuming ref',{$ref:'#/$defs/node',$defs:{node:{$ref:'#/$defs/node'}}});
add('nonconsuming defs',{$defs:{node:{$ref:'#/$defs/node'}}});
add('nonconsuming conditional',{$defs:{node:{if:{$ref:'#/$defs/node'}}}});
add('mutual consuming',{$defs:{a:{properties:{next:{$ref:'#/$defs/b'}}},b:{items:{$ref:'#/$defs/a'}}}});
add('mutual nonconsuming',{$defs:{a:{$ref:'#/$defs/b'},b:{$ref:'#/$defs/a'}}});
add('ref before pattern',{pattern:'(?=a)',$ref:'#/missing'});
add('unknown before ref',{$ref:'#/missing',unknown:true});
add('pattern before alternatives',{allOf:[],pattern:'(?=a)'});
add('alternatives before defs',{$defs:null,anyOf:[null]});
add('canonical before keywords',{unknown:true,default:'a'.repeat(256*1024)});
add('canonical before graph',{items:null,default:9007199254740992});
process.stdout.write(JSON.stringify(rows));
