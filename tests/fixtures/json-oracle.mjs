// Execute the unchanged TypeScript codec as a differential oracle, without a backend build.
import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next) {
  if (specifier.endsWith('.js') && specifier.startsWith('.') && context.parentURL) {
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source))) return next(source.href,context);
  }
  return next(specifier,context);
}});
const {canonical,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {deriveOperationId}=await import('../../packages/contracts/src/operation-id.ts');
const {validateJson}=await import('../../packages/contracts/src/canonical-json.ts');
if(process.argv[2]==='wide') {
  const {createHash}=await import('node:crypto');
  const cases=[];
  for(const count of [399_999,400_000,400_001,450_000]) {
    for(const envelope of [false,true]) {
      const array='['+'null,'.repeat(count-1)+'null]';
      const input=envelope?'\u007b"values":'+array+'}':array;
      const value=JSON.parse(input);
      const encoded=JSON.stringify(value);
      const item={count,envelope,bytes:Buffer.byteLength(encoded),sha256:createHash('sha256').update(encoded).digest('hex')};
      for(const [name,fn] of Object.entries({canonical,validateJson})) {
        try {fn(value,4_000_000);item[name]=null;} catch(error){item[name]=error.code;}
      }
      cases.push(item);
    }
  }
  process.stdout.write(JSON.stringify(cases));
  process.exit(0);
}
if(process.argv[2]==='limits') {
  const cases=[];
  for(const value of [[Number.MAX_SAFE_INTEGER+1],{x:Number.MAX_SAFE_INTEGER+1},[null,Number.MAX_SAFE_INTEGER+1],['😀'],{a:[1,2]}]) {
    for(let maximum=0;maximum<24;maximum++) {
      const item={input:JSON.stringify(value),maximum};
      for(const [name,fn] of Object.entries({canonical,validateJson})) {
        try {fn(value,maximum);item[name]=null;} catch(error){item[name]=error.code;}
      }
      cases.push(item);
    }
  }
  process.stdout.write(JSON.stringify(cases));
  process.exit(0);
}
const values=[null,true,false,0,-0,1e-7,1e-6,0.000123,1.2345678901234567,Number.MIN_VALUE,Number.MAX_SAFE_INTEGER,Number.MAX_SAFE_INTEGER+1,1e20,1e21];
const strings=['plain ASCII','\u0000\n\t"\\','Straße 日本語','😀','\ud800','\udfff','\ud800A\udfff'];
for(let point=0;point<=0x10ffff;point+=997) strings.push(String.fromCodePoint(point));
for(const value of strings) values.push({[value]:[value,true,null,123]});
values.push({'10':1,'2':2,z:3,'𐀀':4,'':5,'01':6,'4294967295':7,'4294967294':8});
let state=42;
for(let i=0;i<1000;i++){state=(Math.imul(state,1664525)+1013904223)>>>0;values.push((state-2147483648)*10**((i%40)-25));}
const results=values.map(value=>{
  const record={input:JSON.stringify(value),stringify:JSON.stringify(value)};
  try { record.canonical=canonical(value); record.hash=hashJson(value);record.operation=deriveOperationId('epoch:00123:nonce😀',value); }
  catch(error){record.error=error.code;}
  return record;
});
process.stdout.write(JSON.stringify(results));
