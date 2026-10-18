import {registerHooks,createRequire} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath,pathToFileURL} from 'node:url';
const require=createRequire(import.meta.url);
const external=new Map(['ajv/dist/2020.js','re2js'].map(s=>[s,pathToFileURL(require.resolve(s)).href]));
registerHooks({resolve(s,c,next){
  if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const source=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,c);}
  if(external.has(s))return next(external.get(s),c);
  return next(s,c);
}});
const {SchemaValidators}=await import('../../packages/contracts/src/schema.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const rows=[];
const values=[null,false,true,0,1,-1,2,0.3,1e21,1e-7,'','a','A','aa','\n','é','e\u0301','😀','\ud800','\udc00',[],[1],[1,1],[1,'1'],['__proto__','__proto__'],[{},{}],[{valueOf:1},{valueOf:1}],{}, {a:1},{a:'x'},{a:1,b:2},{x:1},{x:'s'},{a:1,valueOf:1},{a:1,toString:1},{constructor:1},JSON.parse('{"__proto__":1}')];
function add(schema,inputs=values,pool=null){
  const validator=pool??new SchemaValidators();
  for(const value of inputs){
    let result;try{validator.validate(schema,value);result={ok:true};}catch(error){result={ok:false,wire:IvyError.from(error).toWire()};}
    rows.push({schema:JSON.stringify(schema),value:JSON.stringify(value),result:JSON.stringify(result)});
  }
}
const leaves=[true,false,{},...['null','boolean','integer','number','string','array','object'].map(type=>({type})),{type:[]},{type:['null','string']},{type:''},
  {const:{a:1}},{const:[1,2]},{const:'\ud800'},{const:{constructor:1}}, {enum:[{a:1},'a',1,null]},
  {type:'string',minLength:1,maxLength:2},{pattern:'^a$'}, {pattern:'(?i)k'}, {pattern:'\\p{Cs}'},
  {minimum:1},{maximum:1},{exclusiveMinimum:0},{exclusiveMaximum:1},{multipleOf:0.1},{multipleOf:1},
  {type:'array',minItems:1,maxItems:2}, {uniqueItems:true},{items:{type:'integer'},uniqueItems:true},{items:{type:'string'},uniqueItems:true},
  {properties:{a:{type:'integer'}},required:['a'],additionalProperties:false},{additionalProperties:{type:'string'}},
  {minProperties:1,maxProperties:2},{format:'email'}];
for(const leaf of leaves)add(leaf);
for(const leaf of leaves.slice(0,24))for(const keyword of ['not','items','additionalProperties'])add({[keyword]:leaf});
for(const first of [true,false,{type:'object'},{properties:{a:{type:'integer'}}},{const:{a:1}},{required:['x']}]){
  for(const second of [true,false,{const:{a:1}},{type:'integer'},{properties:{a:{type:'string'}}}]){
    for(const keyword of ['anyOf','oneOf','allOf'])add({[keyword]:[first,second]});
    add({if:first,then:second,else:{type:'string'}});
  }
}
for(const root of [null,'','root','https://example.invalid/a/root','urn:x:root'])for(const leaf of [{type:'string'},{type:'integer'},{properties:{a:{type:'string'}}},{enum:[]},{minimum:'bad'}]){
  add({...(root===null?{}:{$id:root}),$defs:{value:leaf},$ref:'#/$defs/value'});
  add({...(root===null?{}:{$id:root}),$defs:{value:leaf},properties:{a:{$ref:'#/$defs/value'}}});
}
add({$defs:{node:{type:'object',properties:{next:{$ref:'#/$defs/node'},value:{type:'integer'}},additionalProperties:false}},$ref:'#/$defs/node'},[{}, {value:1,next:{value:2}},{next:{value:'bad'}},{next:null}]);
for(const valid of [false,true]){
  let value={value:valid?1:'bad'};for(let i=0;i<127;i++)value={next:value};
  add({$defs:{node:{type:'object',properties:{next:{$ref:'#/$defs/node'},value:{type:'integer'}},additionalProperties:false}},$ref:'#/$defs/node'},[value]);
}
for(const number of [0,-1,0.1,0.3,1,3,1e-30,1e-7,1e20,1e21])add({multipleOf:number},[0,0.3,0.1+0.2,1,3,1e20,1e21,1e22,1e-20]);
// The admission pointer is literal; the compiler's URI pointer decodes %78.
// These are existing admitted behaviors, never a stricter-policy proposal.
const escaped=target=>({$schema:'https://json-schema.org/draft/2020-12/schema#/title',description:{'%78':true,x:target},$ref:'#/description/%78'});
const extended=[
 {dependentRequired:{a:['b']}},{dependencies:{a:['b']}},{dependentSchemas:{a:{required:['b']}}},
 {dependencies:{a:{properties:{b:{type:'integer'}},required:['b']}}},
 {patternProperties:{'^a$':{type:'integer'}},additionalProperties:false},
 {propertyNames:{pattern:'^[a-z]$'}},{prefixItems:[{type:'integer'},{type:'string'}],items:false},
 {contains:{type:'integer'}},{contains:{type:'integer'},minContains:0,maxContains:1},
 {contains:{type:'integer'},minContains:2,maxContains:1},{contains:{pattern:'['},minContains:0},
 {properties:{a:true},unevaluatedProperties:false},{allOf:[{properties:{a:true}}],unevaluatedProperties:false},
 {anyOf:[{properties:{a:true},required:['a']},{properties:{b:true},required:['b']}],unevaluatedProperties:false},
 {if:{properties:{a:{type:'integer'}},required:['a']},then:{properties:{b:true}},else:{properties:{x:true}},unevaluatedProperties:false},
 {prefixItems:[true],unevaluatedItems:false},{contains:{type:'integer'},unevaluatedItems:false},
 {type:'string',nullable:true},{type:['string','null'],nullable:false},{nullable:true},
 {pattern:'(?=a)'},{patternProperties:{'[':true}},{$async:true,type:'string'}, {id:'legacy'},
 {not:{const:{a:1}}},{anyOf:[true,{const:{a:1}}]},{anyOf:[{properties:{a:true}},{const:{a:1}}]},
 {type:'string',minLength:0,const:{a:1}},{type:'string',const:{a:1}},{format:'ignored'},
 {enum:[{constructor:{x:1}}]},{const:{valueOf:1}},{const:{toString:1}},
 {$dynamicAnchor:'node',type:'object',properties:{a:{$dynamicRef:'#node'}}},
 {$recursiveAnchor:true,type:'array',items:{$recursiveRef:'#'}},
];
for(const target of extended)add(escaped(target));
for(const base of [{},{items:true},{additionalProperties:true},{items:true,additionalProperties:true}])add({$defs:{base},$ref:'#/$defs/base',anyOf:[true,{const:{a:1}}]});
for(const target of [
 {patternProperties:{'^a$':true},unevaluatedProperties:false},
 {properties:{a:true},unevaluatedProperties:false},
 {oneOf:[{properties:{a:true},required:['a']},{properties:{b:true},required:['b']}],unevaluatedProperties:false},
 {if:{required:['a']},then:{properties:{b:true}},unevaluatedProperties:false},
 {if:{required:['a']},else:{properties:{b:true}},unevaluatedProperties:false},
])add(escaped(target));
for(const keyword of ['$dynamicRef','$recursiveRef'])for(const name of ['constructor','toString','valueOf','__proto__','hasOwnProperty','isPrototypeOf'])add(escaped({[keyword]:'#'+name}));
for(const keyword of ['type','nullable','enum','required','allOf','anyOf','oneOf','prefixItems','items','contains','additionalProperties','propertyNames','unevaluatedProperties','unevaluatedItems','patternProperties','dependencies','dependentRequired','dependentSchemas','uniqueItems','minContains','maxContains','multipleOf','pattern','$dynamicAnchor','$recursiveAnchor']){
  for(const argument of [null,3,'bad',[],[true],{}, {a:[]},{a:1}])add(escaped({[keyword]:argument}),[null,'a',3,{}, {a:1},[1,1],{a:1,valueOf:1}]);
}
for(const keyword of ['allOf','anyOf','oneOf','prefixItems'])for(const argument of [[3],[null],[[]],[{}],['a']])add(escaped({[keyword]:argument}));
add(escaped({items:{type:''},uniqueItems:true}));
add(escaped({properties:JSON.parse('{"a":true,"b":true,"c":true,"d":true,"e":true,"f":true,"g":true,"h":true,"i":true,"__proto__":false}'),additionalProperties:false}));
add({required:['']},[{}, {'':null}]);
for(const argument of ['',false,0,null,{},[],{toString:1}]){
  add(escaped({required:[argument]}),[{}, {'':null}, {a:1}]);
  add(escaped({required:Array.from({length:64},()=>argument)}),[{}, {'':null}, {a:1}]);
  add(escaped({dependentRequired:{a:[argument]}}),[{}, {a:1}, {a:1,'':null}]);
}
add(escaped({patternProperties:{'[':true},additionalProperties:true}));
add(escaped({patternProperties:{'[':{type:'string'}},additionalProperties:true}));
// An inline dynamic anchor becomes a separate validator when invoked later.
// A missing dynamic target then falls back to that validator's entry schema.
add(escaped({properties:{define:{$dynamicAnchor:'node',type:'object',properties:{x:{$dynamicRef:'#missing'}}},use:{$dynamicRef:'#node'}}}),[{define:{},use:{x:1}},{define:{},use:{x:{}}}]);
add(escaped({type:'array',prefixItems:[{$dynamicAnchor:'node',type:'object',properties:{x:{$dynamicRef:'#missing'}}},{$dynamicRef:'#node'}]}),[[{},{x:[]}],[{},{x:{}}]]);
for(const argument of [JSON.parse('{"__proto__":null}'),JSON.parse('{"__proto__":[]}'),JSON.parse('{"__proto__":["a"]}'),JSON.parse('{"__proto__":[],"length":2,"0":"a","1":"b"}'),JSON.parse('{"__proto__":{},"toString":1}'),JSON.parse('{"__proto__":{"toString":1}}'),[{toString:1}]]){
  add(escaped({required:[argument]}),[{}, {'':null}, {'[object Object]':1}]);
  add(escaped({required:Array.from({length:64},()=>argument)}),[{}, {'':null}, {'[object Object]':1}]);
}
for(const ordered of [false,true]){
  const define={$dynamicAnchor:'node',type:'integer'},use={$dynamicRef:'#node'};
  add(escaped({type:'array',items:{properties:ordered?{define,use}:{use,define}}}),[[{define:1},{use:2}],[{define:1},{use:'x'}],[{use:2},{define:1}]]);
}
for(const keyword of ['dependencies','dependentRequired']){
  const required={a:[JSON.parse('{"__proto__":null}')]};
  add(escaped({properties:{a:{type:'integer'}},[keyword]:required}),[{a:'x'},{a:1}]);
  add(escaped({additionalProperties:false,[keyword]:required}),[{a:1}]);
}
for(const source of [{properties:{a:{type:'integer'},b:{const:{a:1}}}},{properties:{b:{const:{a:1}},a:{type:'integer'}}}])add(source,[{a:'bad',b:{a:1,valueOf:1}}]);
// One shared cache must retain the first compilation order for equal canonical
// schemas, including schemas resubmitted with reordered property insertion.
const pool=new SchemaValidators();
const first={properties:{a:{type:'integer'},b:{const:{a:1}}}},second={properties:{b:{const:{a:1}},a:{type:'integer'}}};
add(first,[{a:'bad',b:{a:1,valueOf:1}}],pool);add(second,[{a:'bad',b:{a:1,valueOf:1}}],pool);
rows.at(-2).pool='ordered';rows.at(-1).pool='ordered';
process.stdout.write(JSON.stringify(rows));
