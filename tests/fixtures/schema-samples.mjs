// Representative values and mutations for checked-in schema differential tests.
export function schemaSamples(ajv,documents,selected=documents){
const byId=new Map(documents.map(document=>[document.$id,document]));
const strings=['a','x','1.0.0','service:a','TASK-0001','2026-01-01','00:00','2026-01-01T00:00:00.000Z','sha256:'+'a'.repeat(64),'a'.repeat(40),'a'.repeat(96),'a-b','dist/apps/example','index.html','path/file','asdk_app_'+'a'.repeat(32),'objects','https://example.invalid/','application/json','object.id','/settings/key','/key','a!b','1','+491234567890','sip:a@example.invalid','*1#','ivynext'+'a'.repeat(24)+'.slice','00000000-0000-0000-0000-000000000000'];
function sample(schema,base,trail=new Set(),depth=0){
  if(schema===true)return null;if(schema===false||depth>12)return null;
  if(schema.$ref){const url=new URL(schema.$ref,base),document=byId.get(url.origin+url.pathname);let child=document;
    for(const key of url.hash.slice(2).split('/'))child=child?.[key.replaceAll('~1','/').replaceAll('~0','~')];
    if(!child||trail.has(child))return null;return sample(child,url.origin+url.pathname,new Set(trail).add(child),depth+1);
  }
  if(Object.hasOwn(schema,'const'))return schema.const;if(schema.enum)return schema.enum[0];
  for(const keyword of ['anyOf','oneOf'])if(schema[keyword]){for(const child of schema[keyword]){const value=sample(child,base,trail,depth+1);try{if(ajv.compile({...child,$id:undefined})(value))return value;}catch{}}return sample(schema[keyword][0],base,trail,depth+1);}
  const type=Array.isArray(schema.type)?schema.type[0]:schema.type??(schema.properties?'object':undefined);
  let value=null;
  if(type==='object'){value={};for(const key of schema.required??[])Object.defineProperty(value,key,{value:sample(schema.properties?.[key]??{},base,trail,depth+1),enumerable:true,writable:true,configurable:true});}
  else if(type==='array')value=Array.from({length:schema.minItems??0},()=>sample(schema.items??{},base,trail,depth+1));
  else if(type==='string'){const re=schema.pattern?new RegExp(schema.pattern,'u'):null;value=strings.find(text=>[...text].length>=(schema.minLength??0)&&[...text].length<=(schema.maxLength??Infinity)&&(!re||re.test(text)))??'x'.repeat(schema.minLength??0);}
  else if(type==='number'||type==='integer'){value=schema.minimum??(schema.exclusiveMinimum!==undefined?schema.exclusiveMinimum+1:0);if(schema.multipleOf)value=Math.ceil(value/schema.multipleOf)*schema.multipleOf;}
  else if(type==='boolean')value=false;
  if(schema.allOf){for(const child of schema.allOf){const extra=sample(child,base,trail,depth+1);if(value&&extra&&typeof value==='object'&&typeof extra==='object'&&!Array.isArray(value)&&!Array.isArray(extra))Object.assign(value,extra);}}
  return value;
}
const rows=[];
for(const document of selected)for(const [name,schema]of Object.entries(document.$defs??{})){
  const reference=document.$id+'#/$defs/'+name,validate=ajv.getSchema(reference),seed=sample(schema,document.$id);
  const values=[null,false,0,'',[],{},seed,'\ud800','😀'];
  if(seed&&typeof seed==='object'&&!Array.isArray(seed)){
    values.push({...seed,unexpected:true});
    for(const key of Object.keys(seed).slice(0,4)){const missing={...seed};delete missing[key];values.push(missing);values.push({...seed,[key]:'\ud800'});}
  }
  for(const value of values)rows.push({reference,input:JSON.stringify(value),valid:Boolean(validate(value))});
  for(const input of ['1e400','-1e400','9007199254740992'])rows.push({reference,input,valid:Boolean(validate(JSON.parse(input)))});
}
return rows;
}
