import {readFileSync} from 'node:fs';
import {Ajv2020} from 'ajv/dist/2020.js';
const document=JSON.parse(readFileSync(new URL('./schema-reference.schema.json',import.meta.url),'utf8'));
const inputs=[null,false,true,0,17,'','x','accent','astral','replacement','surrogate','percent','malformed','encoded slash','hash','empty','literal tilde',[],[1],{}];
const pointers=[
  '', '/',
  '/$defs/plain','/$defs/%70lain','/$defs/a~1b','/$defs/a%7E1b','/$defs/a%2Fb','/$defs/a~0b',
  '/$defs/é','/$defs/%C3%A9','/$defs/😀','/$defs/%F0%9F%98%80','/$defs/�',
  '/$defs/%ED%A0%80','/$defs/%EF%BF%BD','/$defs/%25','/$defs/%','/$defs/%ZZ',
  '/$defs/%2','/$defs/%252F','/$defs/%25ZZ','/$defs/#','/$defs/%23','/$defs/?',
  '/$defs/','/$defs/~01','/$defs/%7e01','/$defs/escapedAlias','/$defs/surrogateAlias',
  '/$defs/percentAlias','/$defs/malformedAlias','/$defs/prototypeAlias','/$defs/missing','/$defs/%FF',
  '/$defs/%C0%AF','/$defs/%F4%90%80%80','/$defs/%E2%82','%2F$defs/plain',
  '/$defs/a~1b/type','/$defs/nested/const/rows/0','/$defs/nested/const/rows/1',
  '/$defs/nested/const/rows/01','/$defs/nested/const/rows/-0','/$defs/nested/const/rows/length',
  '/$defs/nested/const/text/length','/$defs/nested/const/text/0','/$defs/nested/const/nil',
  '/$defs/nested/const/flag/constructor','/$defs/nested/const/number/toString',
];
for(const path of ['/$defs/plain','/$defs/nested/const/rows','/$defs/nested/const/text']){
  for(const tail of ['constructor','constructor/name','constructor/length','constructor/caller',
    'constructor/prototype','__proto__','__proto__/constructor','__proto__/__proto__',
    '__proto__/__proto__/__proto__','hasOwnProperty','toString'])pointers.push(path+'/'+tail);
}
const rows=[];
for(const pointer of pointers){
  const reference=document.$id+'#'+pointer;
  // Isolate compilation failures and prototype lookups from other cases.
  const ajv=new Ajv2020({strict:false,allErrors:false,ownProperties:true,inlineRefs:false,validateFormats:false});
  ajv.addSchema(document);
  let validator;
  try{validator=ajv.getSchema(reference);}catch{}
  for(const input of inputs){
    let result;
    try{if(!validator)throw Error();result={ok:true,valid:validator(input)};}
    catch{result={ok:false};}
    rows.push({reference,input:JSON.stringify(input),result});
  }
}
process.stdout.write(JSON.stringify(rows));
