const units=value=>Array.from({length:value.length},(_,i)=>value.charCodeAt(i));
const mappings=[],contexts=[];
let scalarCount=0;
const add=input=>contexts.push({input:units(input),output:units(input.toLocaleLowerCase('en'))});
for(let point=0;point<=0x10ffff;point++){
  if(point>=0xd800&&point<=0xdfff)continue;
  const input=String.fromCodePoint(point),output=input.toLocaleLowerCase('en');
  scalarCount++;
  if(output!==input)mappings.push([point,units(output)]);
  if(/\p{Case_Ignorable}/u.test(input)){
    add('AΣ'+input);
    add('AΣ'+input+'A');
    add('A'+input+'Σ');
  }
}
for(let unit=0xd800;unit<=0xdfff;unit++){
  const boundary=String.fromCharCode(unit);
  for(const input of ['A'+boundary+'Σ','AΣ'+boundary,boundary+'İΣ',boundary+'𐐀'+boundary+'ΟΣ'])add(input);
}
for(const input of ['', 'ABC', 'ΟΣ', 'ΟΣΑ', 'AΣ', 'Σ', 'ΣΑ', 'AΣ.A', 'AΣ:A', 'I\u0307', 'İ', 'I\u0301', 'ẞß', '𐐀𐐨', '𞤀𞤢', '\ud800\udfffΣ', 'AΣ\u0000A'])add(input);
process.stdout.write(JSON.stringify({node:process.version,unicode:process.versions.unicode,icu:process.versions.icu,scalarCount,mappings,contexts}));
