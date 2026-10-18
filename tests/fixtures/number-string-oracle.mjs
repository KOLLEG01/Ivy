const strings=['',' ','0','-0','+0','Infinity','-Infinity','+Infinity','inf','NaN','1_000','0x','0o','0b','.','+.','1e','1e+','0x1p2','0Xff','\ud800','1\0','  1  ','1e999999','-1e-999999','2.4703282292062327e-324','2.4703282292062328e-324','1.7976931348623157e308','1.7976931348623159e308'];
for(let u=0;u<=0xffff;u++)strings.push(String.fromCharCode(u)+'1'+String.fromCharCode(u));
for(const n of [0,1,2,52,53,54,55,63,64,100,1023,1024,1025,4096])for(const delta of [-3n,-2n,-1n,0n,1n,2n,3n]){
 const value=(1n<<BigInt(n))+delta;if(value<0n)continue;
 for(const [radix,prefix] of [[2,'0b'],[8,'0o'],[16,'0x']])strings.push(prefix+value.toString(radix));
 strings.push(value.toString(),'-'+value.toString());
}
let seed=42;function random(){seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed;}
for(let i=0;i<2000;i++){
 const digits=Array.from({length:1+random()%100},()=>String(random()%10)).join('');
 strings.push((i%2?'-':'+')+digits.slice(0,1)+'.'+digits.slice(1)+'e'+(random()%800-400));
 const hex=Array.from({length:1+random()%300},()=>'0123456789abcdef'[random()%16]).join('');strings.push('0x'+hex);
}
const tag=n=>{if(Number.isNaN(n))return 'nan';const bytes=Buffer.alloc(8);bytes.writeDoubleBE(n);return bytes.toString('hex');};
process.stdout.write(JSON.stringify(strings.map(s=>[JSON.stringify(s),tag(Number(s))])));
