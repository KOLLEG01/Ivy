import traverse from 'json-schema-traverse';
import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n){
  if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){
    const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);
    if(existsSync(fileURLToPath(u)))return n(u.href,c);
  }
  return n(s,c);
}});
const {admitSchema}=await import('../../packages/contracts/src/schema.ts');
const rows=[];
function containsId(value){
  return value!==null&&typeof value==='object'&&
    (Object.hasOwn(value,'$id')||Object.values(value).some(containsId));
}
function add(name,schema){
  admitSchema(schema);
  const nodes=[];
  traverse(schema,{allKeys:true},(node,pointer,root,parent)=>{
    nodes.push({pointer,parent:parent??null,schema:JSON.stringify(node)});
  });
  rows.push({name,schema:JSON.stringify(schema),result:JSON.stringify({isolated:containsId(schema),nodes})});
}
add('true',true);add('false',false);add('anonymous',{});
for(const id of [null,false,0,'','#','#/','a','https://example.invalid/root','\ud800',[],{}])
  add('root id '+JSON.stringify(id),{$id:id});
const leaf={$id:'resource',type:'string'};
const maps=['$defs','definitions','properties','patternProperties','dependencies'];
const arrays=['items','allOf','anyOf','oneOf'];
const skipped=['default','enum','const','required','maximum','minimum','exclusiveMaximum',
  'exclusiveMinimum','multipleOf','maxLength','minLength','pattern','format','maxItems',
  'minItems','uniqueItems','maxProperties','minProperties'];
const inherited=Object.getOwnPropertyNames(Object.prototype);
const keys=[...new Set([...maps,...arrays,...skipped,...inherited,'title','description',
  'examples','if','then','else','not','additionalProperties','propertyNames','contains',
  'additionalItems','custom','0','2','10','01','a/b','a~b','', '\ud800'])];
for(const key of keys){
  for(const value of [leaf,[leaf],{x:leaf},true,null]){
    // Annotations are admitted without interpreting their contents as schemas.
    const annotation=JSON.parse(JSON.stringify({[key]:value}));
    for(const wrapper of ['description','default','examples','const'])
      add(wrapper+'/'+key+'/'+JSON.stringify(value),{[wrapper]:annotation});
  }
}
for(const name of ['a/b','a~b','', '0','10','2','01','\ud800','\udfff']){
  for(const keyword of ['$defs','properties'])
    add(keyword+'/'+name,{[keyword]:{[name]:{description:{nested:leaf}}}});
}
add('schema parents',{$defs:{a:leaf},properties:{x:{items:{allOf:[leaf,true,false]}}}});
add('enumeration order',{description:JSON.parse('{"10":{"$id":"ten"},"2":{"$id":"two"},"b":{"$id":"b"},"01":{"$id":"one"},"a":{"$id":"a"}}')});
add('id inside ignored arrays',{examples:[{deep:{$id:null}}]});
add('colliding annotation paths',{description:{'a/b':leaf,a:{b:leaf}}});
process.stdout.write(JSON.stringify(rows));
