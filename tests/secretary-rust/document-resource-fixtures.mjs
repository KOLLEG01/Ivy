// Finite resource inputs. Saved parser cases are reused; no oracle is rerun.
import {readFileSync,writeFileSync,mkdirSync,existsSync} from 'node:fs';
import {resolve,join} from 'node:path';
import {crc32,deflateRawSync} from 'node:zlib';
import {createHash} from 'node:crypto';
import {pdfBytes} from '../secretary-pdf-fixture.mjs';
const directory=resolve(process.argv[2]);
if(existsSync(directory))throw Error('Original resource corpus exists; reconcile it');
mkdirSync(directory);
const sha=bytes=>'sha256:'+createHash('sha256').update(bytes).digest('hex');
const word=text=>Buffer.from('<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>'+text+'</w:t></w:r></w:p></w:body></w:document>');
// Two-member ZIP fixture, using only the existing Node built-in CRC/deflate.
function docx(text){
  const parts=[['[Content_Types].xml',Buffer.from('<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')],['word/document.xml',word(text)]];
  const local=[],central=[];let offset=0;
  for(const[name,bytes]of parts){
    const filename=Buffer.from(name),data=deflateRawSync(bytes,{level:9}),crc=crc32(bytes);
    const header=Buffer.alloc(30);header.writeUInt32LE(0x04034b50);header.writeUInt16LE(20,4);header.writeUInt16LE(8,8);header.writeUInt32LE(crc,14);header.writeUInt32LE(data.length,18);header.writeUInt32LE(bytes.length,22);header.writeUInt16LE(filename.length,26);
    local.push(header,filename,data);
    const entry=Buffer.alloc(46);entry.writeUInt32LE(0x02014b50);entry.writeUInt16LE(20,4);entry.writeUInt16LE(20,6);entry.writeUInt16LE(8,10);entry.writeUInt32LE(crc,16);entry.writeUInt32LE(data.length,20);entry.writeUInt32LE(bytes.length,24);entry.writeUInt16LE(filename.length,28);entry.writeUInt32LE(offset,42);
    central.push(entry,filename);offset+=header.length+filename.length+data.length;
  }
  const index=Buffer.concat(central),end=Buffer.alloc(22);end.writeUInt32LE(0x06054b50);end.writeUInt16LE(parts.length,8);end.writeUInt16LE(parts.length,10);end.writeUInt32LE(index.length,12);end.writeUInt32LE(offset,16);
  return Buffer.concat([...local,index,end]);
}
const cases=[];
function add(name,format,bytes,expected,extra={}){
  bytes=Buffer.isBuffer(bytes)?bytes:Buffer.from(bytes);
  const file=name+'.bin';writeFileSync(join(directory,file),bytes,{flag:'wx'});
  cases.push({name,format,file,byteLength:bytes.length,contentHash:sha(bytes),budget:6*1024*1024,expected,targets:format==='pdf'?['pdf']:['rust','original'],...extra});
}
add('html-original-inert','html','<h1>Original 🌍</h1><p>Inert &amp; complete</p><script>private</script>','accepted');
add('html-encoding','html','<meta charset=windows-1252><p>text</p>','media_text_encoding_unsupported');
add('html-empty','html','<script>not text</script>','media_document_no_text');
add('html-native-budget','html','<p>inert</p>','media_native_input_too_large',{budget:1});
add('html-five-mib-text','html','<p>'+'x'.repeat(5*1024*1024)+'</p>','accepted');
add('html-source-over-limit','html',Buffer.alloc(32*1024*1024+1,120),'media_document_too_large');
add('html-node-heavy','html','<p>x</p>'.repeat(100000),'accepted');
add('html-node-over-limit','html','<p>x</p>'.repeat(125001),'media_document_too_large');
const saved=JSON.parse(readFileSync(new URL('./document-worker-fixtures.json',import.meta.url),'utf8'));
for(const [i,name,expected]of [[0,'plain','accepted'],[1,'parts','accepted'],[2,'unclosed','media_document_invalid'],[3,'doctype','media_document_invalid']])add('docx-original-'+name,'docx',Buffer.from(saved.docx[i],'base64'),expected);
add('docx-five-mib-text','docx',docx('x'.repeat(5*1024*1024)),'accepted');
add('docx-expanded-over-limit','docx',docx('x'.repeat(16*1024*1024)),'media_document_too_large');
add('pdf-original-pages','pdf',pdfBytes(),'accepted');
add('pdf-original-empty','pdf',pdfBytes(['']),'media_document_no_text');
add('pdf-original-mixed','pdf',pdfBytes(['First','']),'accepted');
add('pdf-signature','pdf',Buffer.from('not PDF'),'media_signature_mismatch');
add('pdf-malformed','pdf',Buffer.from('%PDF-1.4\nnot a valid document'),'media_document_invalid');
add('pdf-page-limit','pdf',pdfBytes(Array.from({length:128},(_,i)=>'Finite page '+(i+1))),'accepted');
add('pdf-page-over-limit','pdf',pdfBytes(Array.from({length:129},(_,i)=>'Finite page '+(i+1))),'media_document_too_large');
add('rust-missing-input-eof','html','<p>inert</p>','owner_deadline',{targets:['rust'],holdEof:true});
const manifest={schemaVersion:1,cases,inputLimit:32*1024*1024,replyLimit:8*1024*1024,deadlineMs:30000,originalV8:{maxOldGenerationSizeMb:128,maxYoungGenerationSizeMb:16,stackSizeMb:4},qualification:'Resource observations and existing outcome expectations; not a new semantic oracle, memory-exhaustion test or OS-cap qualification.'};
writeFileSync(join(directory,'manifest.json'),JSON.stringify(manifest)+'\n',{flag:'wx'});
console.log(JSON.stringify({cases:cases.length,workloads:cases.reduce((n,c)=>n+c.targets.length,0),inputBytes:cases.reduce((n,c)=>n+c.byteLength,0),manifestSha256:sha(readFileSync(join(directory,'manifest.json')))}));
