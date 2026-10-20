import assert from 'node:assert/strict';
assert.equal(process.versions.node, '24.18.0');
const units = text => Array.from({length:text.length}, (_, i) => text.charCodeAt(i));
let count=0, failures=0;
function emit(input) {
  const row={input:units(input),bytes:Buffer.byteLength(input)};
  try {
    const parsed=JSON.parse(input);
    row.value=JSON.stringify(parsed);
    if(typeof parsed==='number') { const bytes=Buffer.alloc(8);bytes.writeDoubleBE(parsed);row.numberBits=bytes.toString('hex'); }
  } catch(error) { row.name=error.name;row.message=units(error.message);failures++; }
  process.stdout.write(JSON.stringify(row)+'\n');count++;
}
const seeds=['','{','[','[invalid]','undefined','NaN','Infinity','[object Object]',
  'null','true','false','-0','0.1','9007199254740993','1e400','-1e400','1e-4000',
  '"text"','"é😀\\uD800"','{"1":0,"01":2,"1":3,"__proto__":4}',
  '[null,true,false,{"key":[1,-0,2e30,"\\uDC00"]}]',
  ' \r\n {\r\n "é😀": [true, false],\r "next": null\n}\t ',
  '"\\界"','"\\é"','"\\u123"','"\\u12X4"','"\\',
  '"\u0000"','{"x":0,}','[0,]','01','-01','1.e','1e+'];
for(const text of seeds) { emit(text);for(let n=0;n<text.length;n++)emit(text.slice(0,n)); }
// Every raw UTF-16 unit is tested as an unexpected token and string content.
// This includes control characters, all BMP values and both surrogate ranges.
for(let unit=0;unit<=0xffff;unit++) {
  const text=String.fromCharCode(unit);emit(text);emit('"'+text+'"');
  if(unit<512 || unit%31===0 || (unit>=0xd7ff&&unit<=0xd801) || (unit>=0xdbff&&unit<=0xdc01))emit('"\\'+text+'"');
}
const replacements=['x','"','\\','\n','\r',' ',':',',','[',']','{','}','0','1','-','e','t','f','n','\ud800','\udfff','😀'];
for(const text of seeds)for(let at=0;at<text.length;at++)for(const replacement of replacements)emit(text.slice(0,at)+replacement+text.slice(at+1));
for(const length of [19,20,21,22,30,40,41,60])for(let at=0;at<length;at++) {
  emit(' '.repeat(at)+'x'+('😀\ud800é').repeat(length).slice(0,length-at-1));
  emit(' '.repeat(at)+'\ud800'+'z'.repeat(length-at-1));
}
let state=0x1847ace;
const alphabet=units(' \t\r\n[]{}:,"\\/-+0123456789.eEtnruefals').concat([0,127,128,255,256,0x2028,0x2029,0xd800,0xdc00,0xffff]);
const next=()=>{state^=state<<13;state^=state>>>17;state^=state<<5;return state>>>0;};
for(let n=0;n<8192;n++) { let text='';const length=next()%49;for(let i=0;i<length;i++)text+=String.fromCharCode(alphabet[next()%alphabet.length]);emit(text); }
process.stdout.write(JSON.stringify({summary:{node:process.versions.node,v8:process.versions.v8,count,failures}})+'\n');
