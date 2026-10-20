const hex=bytes=>bytes.toString('hex');
const decode=input=>{const bytes=Buffer.from(input,'base64url');return {bytes:hex(bytes),text:bytes.toString('utf8')};};
const canonical=input=>{const bytes=Buffer.from(input,'base64');return bytes.toString('base64')===input?hex(bytes):null;};
const contexts=['XQ==','QX==','QQX','AAAXAA==','QUJDX-_+/AA='];
const units=contexts.map(context=>{
 const expected=Array.from({length:256},(_,unit)=>decode(context.replace('X',String.fromCharCode(unit))));
 for(let unit=0;unit<=0xffff;unit++){
  const observed=decode(context.replace('X',String.fromCharCode(unit))),same=expected[unit&255];
  if(observed.bytes!==same.bytes||observed.text!==same.text)throw Error('Node low-byte class differs '+unit+' '+context);
 }
 return {context,expected};
});
const utf8=[];
for(let first=0;first<=255;first++){
 const bytes=Buffer.from([first]);utf8.push([bytes.toString('base64url'),bytes.toString('utf8')]);
 for(let second=0;second<=255;second++){const bytes=Buffer.from([first,second]);utf8.push([bytes.toString('base64url'),bytes.toString('utf8')]);}
}
const edgeBytes=['','00','efbbbf','eda080','edb080','c0af','e08080','e09fbf','e0a080','eda08061','ed9fbf','ee8080','f0808080','f08fbfbf','f0908080','f48fbfbf','f4908080','f5808080','f09f92','e282','e24180','f09f4192','f09f9241','80bfc1f5ff','6162006364'];
for(const value of edgeBytes){const bytes=Buffer.from(value,'hex');utf8.push([bytes.toString('base64url'),bytes.toString('utf8')]);}
const alphabet=['A','B','Z','a','z','0','9','+','/','-','_','=','?','!',' ',String.fromCharCode(0),'\n','\r','\t','\u0141','\u013d','\ud841','\udc41','😀'];
const inputs=new Set(['','A','AA','AAA','AAAA','AAB','AAB=','AA=A','A=A','A==A','AA===AA','QQ==ignored','+-/_',' AB-_+/!?','77u/','7aCA','8ICA','wK8']);
for(const first of alphabet)for(const second of alphabet)for(const third of alphabet)inputs.add(first+second+third);
for(let remainder=0;remainder<4;remainder++)for(const pad of ['','=','==','===','=Z','==AA','!','\u013d','\ud83d'])inputs.add('QUJD'.repeat(32)+'A'.repeat(remainder)+pad);
let state=0x85ba64;
const random=max=>{state=(Math.imul(state,1664525)+1013904223)>>>0;return state%max;};
for(let i=0;i<8192;i++){let input='';const count=random(80);for(let j=0;j<count;j++)input+=alphabet[random(alphabet.length)];inputs.add(input);}
const malformed=[...inputs].map(input=>({input:JSON.stringify(input),...decode(input)}));
const canonicalInputs=new Set(inputs);
for(let first=0;first<=255;first++){
 canonicalInputs.add(Buffer.from([first]).toString('base64'));
 for(let second=0;second<=255;second++)canonicalInputs.add(Buffer.from([first,second]).toString('base64'));
}
const canonicalUnits=contexts.map(context=>({context,expected:Array.from({length:256},(_,unit)=>canonical(context.replace('X',String.fromCharCode(unit))))}));
for(const {context,expected}of canonicalUnits)for(let unit=0;unit<=0xffff;unit++){
 const actual=canonical(context.replace('X',String.fromCharCode(unit)));
 if(actual!==(unit<=255?expected[unit]:null))throw Error('Canonical UTF16 class differs '+unit+' '+context);
}
const canonicalRows=[...canonicalInputs].map(input=>({input:JSON.stringify(input),expected:canonical(input)}));
const maximumBytes=8*1024*1024;
const limits=[...new Set([0,1,2,3,maximumBytes-1,maximumBytes,maximumBytes+1])].map(length=>{
 const bytes=Buffer.alloc(length),encoded=bytes.toString('base64'),decoded=Buffer.from(encoded,'base64');
 return {length,accepted:decoded.length<=maximumBytes&&decoded.toString('base64')===encoded};
});
process.stdout.write(JSON.stringify({node:process.version,unitComparisons:contexts.length*65536,units,utf8,malformed,canonicalRows,canonicalUnits,limits}));
