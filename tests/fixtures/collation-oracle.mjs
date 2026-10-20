import assert from 'node:assert/strict';
assert.equal(process.versions.icu, '78.3', 'This differential fixture requires the pinned ICU baseline.');
const units = text => Array.from({length:text.length}, (_,i) => text.charCodeAt(i));
const edge = ['', ...Array.from({length:128}, (_,i)=>String.fromCharCode(i)),
  'é','e\u0301','a\u034f','a\u200d','ä','Å','A\u030a','Æ','æ','Œ','ß','ss',
  'I','İ','ı','ö','z','ch','ll','10','2','001','01','ไทย','日本','が','か\u3099',
  '😀','👩\u200d💻','\ud800','\udfff','\ufffd','x\ud800y','x\ufffdy','a\0b'];
for(const locale of ['default','en']) {
  const compare = (a,b) => Math.sign(locale==='default'?a.localeCompare(b):a.localeCompare(b,locale));
  const row = (a,b) => process.stdout.write(JSON.stringify([locale,units(a),units(b),compare(a,b)])+'\n');
  for(const a of edge) for(const b of edge) row(a,b);
  for(let unit=0;unit<=0xffff;unit++) {
    const a=String.fromCharCode(unit);
    row(a,'\ufffd');
    row('a'+a+'z','a'+String.fromCharCode((unit+1)&0xffff)+'z');
  }
  let state=0x7885;
  const random=max=>{state=(Math.imul(state,1664525)+1013904223)>>>0;return state%max;};
  for(let i=0;i<4096;i++) {
    const word=()=>Array.from({length:random(8)},()=>edge[random(edge.length)]).join('');
    row(word(),word());
  }
}
