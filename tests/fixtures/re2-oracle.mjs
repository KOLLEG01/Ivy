import {registerHooks} from 'node:module';
import {readFileSync,existsSync,readdirSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {RE2JS} from 're2js';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {admitSchema}=await import('../../packages/contracts/src/schema.ts');
const patterns=new Set(['','a','ab','a|b','a||b','|','()','(?:)','(?i)','(?-)','(?-i)','(?i-m:s)','(?i-i)','(?q)','(?i-)','(?P<a>a)(?P<a>b)','(?<1>a)','(?<é>a)','(?<=a)','(?<!a)','(?=a)','(?!a)','(?>a)','(?i:a)b','a*(?i)*','^*','a{*','{*','a{01}','a{1,01}','a{1001}','a{999999999}','a{1,0}','a{2,}','a{,2}','(a{100}){10}','(a{100}){11}','(a{100}){0}','[]','[^]','[]a]','[a&&b]','[a--b]','[a~~b]','[a-z]','[^a]','[a-]','[-a]','[\\d-\\w]','[a-\\d]','[[:alpha:]]','[[:missing:]]','[[:alpha]','\\','\\u0041','\\x41','\\x{D800}','\\x{10FFFF}','\\x{110000}','\\x{}','\\x{000000041}','\\Q.*\\E+','\\Q\\E*','\\Qunterminated','\\E','\\0','\\00','\\000','\\1','\\10','\\1234','\\8','\\_','\\!','\\é','\\pL','\\p{^L}','\\P{^L}','\\p{Script=Latin}','\\p{Letter}','\\p{ASCII}','(?i)\\p{Ascii}','(?i)[a-z]','(?i)[^a-z]','(?i)k','(?i)ſ','(?i)ß','\\A.*\\z','a$','(?m)^a$','\\bword\\b','\\B','\\d','\\D','\\s','\\S','\\w','\\W','(?i)\\w','(?i)\\W','\\p{Cs}','\\p{Tolong_Siki}','.', '^.$','^..$','(?s)^.$','\ud800','\udc00','😀','[\ud800-\udfff]','[😀-😃]','[a\ud800]']);
for(const atom of ['a','.','[a-z]','[^a]','(?:a|ab)','(a?)','^','\\b','\\p{L}','(?i:k)','\\Qab\\E'])for(const suffix of ['*','+','?','??','*?','+?','**','+*','{0}','{1}','{2}','{0,2}','{2,3}','{2,}','{0,1000}','{1000}?'])patterns.add(atom+suffix);
const tables=JSON.parse(readFileSync(new URL('../../packages/contracts/rust/src/re2-data.json',import.meta.url),'utf8'));
for(const name of Object.keys(tables.groups)){const pattern=name.startsWith('\\')?name:name.startsWith('[:')?'['+name+']':'\\p{'+name+'}';patterns.add(pattern);patterns.add('(?i)'+pattern);}
function collect(value){if(!value||typeof value!=='object')return;for(const [key,item] of Object.entries(value)){if(key==='pattern'&&typeof item==='string')patterns.add(item);else collect(item);}}
for(const name of readdirSync(new URL('../../specs/schemas/',import.meta.url)).filter(name=>name.endsWith('.json')))collect(JSON.parse(readFileSync(new URL('../../specs/schemas/'+name,import.meta.url),'utf8')));
let seed=0x13579;
const random=n=>{seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed%n;};
const alphabet='abc012{}[]()|?*+\\.^$-:P<>imsU';
for(let i=0;i<1800;i++){let pattern='';for(let n=random(24);n>0;n--)pattern+=alphabet[random(alphabet.length)];patterns.add(pattern);}
const atoms=['a','b','.','[a-z]','[\\d-\\w]','[^x]','\\d','\\w','\\p{L}','\\x{D800}','(?i:k)'];
const suffixes=['','*','+','?','{0,2}','{2}','{1,3}','{0}'];
for(let i=0;i<400;i++){const a=atoms[random(atoms.length)],b=atoms[random(atoms.length)];patterns.add('^(?:'+a+suffixes[random(suffixes.length)]+'|'+b+')'+suffixes[random(suffixes.length)]+'$');}
const inputs=['','a','b','ab','aba','aaab','abc','a\nb','a\r\nb','a\n','A','k','K','K','s','S','ſ','ß','ẞ','a&b','&','-','.*','word',' word ','1','123','١','\t','\n','\v','\u00a0','\u2028','\ud800','\udc00','😀','😃','\u{11db0}','\u{10ffff}','\0','A\0B'];
for(const pattern of ['((a{100}){0,}){11}','((a{100}){0}){11}','((a{100})*){11}','((a{100}){1,}){11}','((a{100}){0,}){10}'])patterns.add(pattern);
const rows=[];
function add(pattern,values=inputs){let result;try{admitSchema({pattern});const expression=RE2JS.compile(pattern);result={ok:true,matches:values.map(input=>({input:JSON.stringify(input),valid:expression.matcher(input).find()}))};}catch(error){result={ok:false,code:error.code,message:error.message};}rows.push({pattern:JSON.stringify(pattern),...result});}
for(const pattern of patterns)add(pattern);
for(const count of [63,128,400,500]){add('('.repeat(count)+'a'+')'.repeat(count),['a','b','']);add('(?:a+)'.replace('a','('.repeat(count)+'a'+')+'.repeat(count)),['a','aa','b']);}
add('a'.repeat(1024),['a'.repeat(1024),'a'.repeat(1023)]);add('a'.repeat(1025),['']);
add('^(a+)+b$',['a'.repeat(20000),'a'.repeat(20000)+'b']);
add('^(a|aa)*b$',['a'.repeat(20000),'a'.repeat(20000)+'b']);
add('a{1000}',['a'.repeat(999),'a'.repeat(1000)]);
process.stdout.write(JSON.stringify(rows));
