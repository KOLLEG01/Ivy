import {win32} from 'node:path';
const inputs=new Set(['','.','..','/','\\','C:','C:/','C:\\','//host/share','\\\\host\\share','CONX','CON','CON:','NUL_','foo:','foo:a','foo:/','a/../C:/x','a:b','LPT¹X']);
const roots=['','C:','c:','C:/','C:\\','/','\\','//','\\\\','//host/share','\\\\host\\share','\\\\?\\C:','\\\\?\\UNC\\host\\share','\\\\.\\COM1:','\\\\?\\COM¹:'];
const parts=['','a','A','.','..','...','c:','foo:','NUL','CONX','COM1:','İ','ΟΣ','\ud800','\udfff'];
for(const root of roots)for(const separator of ['/','\\','/\\'])for(const first of parts)for(const second of parts)inputs.add(root+separator+first+separator+second);
for(const root of roots)for(const suffix of ['','/','\\','//','\\\\','/./','/../','/a/../../','/a/.../','/Σ/','/😀/','/\ud800/','/\udfff/','/a\0b/'])inputs.add(root+suffix);
let state=0x8b5;
const random=max=>{state=(Math.imul(state,1664525)+1013904223)>>>0;return state%max;};
const alphabet=['a','A','.','..','c:',':','CON','CONX','NUL:','/','\\','\\\\','?','é','Σ','\u0301','\ud800','\udc00','\0',' '];
for(let i=0;i<3000;i++){let value='';const count=1+random(9);for(let j=0;j<count;j++)value+=alphabet[random(alphabet.length)];inputs.add(value);}
const rows=[...inputs].map(input=>{const normalized=win32.normalize(input);return {input:JSON.stringify(input),normalized:JSON.stringify(normalized),key:JSON.stringify(normalized.toLowerCase())};});
process.stdout.write(JSON.stringify({node:process.version,unicode:process.versions.unicode,icu:process.versions.icu,rows}));
