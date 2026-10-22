import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {load} from './inbox-fixtures.mjs';
const {extractDocument}=await load('services/secretary/src/document-extract.js');
const {hashJson,digest}=await load('packages/sdk/src/node.js');
// Fixed bytes from the existing document fixture; the native oracle needs no ZIP writer.
const fixture=new URL('./document-worker-fixtures.json',import.meta.url),saved=JSON.parse(readFileSync(fixture,'utf8'));
const [plain,parts,unclosed,doctype]=saved.docx.map(value=>Buffer.from(value,'base64'));
const executable=process.argv[2];assert(executable);const cases=[],failures=[];
const fixtures=[['html','<h1>Original 🌍</h1><p>Inert &amp; complete</p><script>private</script>'],['html','<table><p>fostered<td>A<td>B</table><b>unclosed<i>italic &amp;'],['html','<svg><text>Shape</text></svg><template>hidden</template>'],['html','\ufeff<p>visible\u0085</p>'],['html','<meta charset=windows-1252><p>text</p>'],['html',Buffer.from([255])],['html','<script>not text</script>'],['docx',plain],['docx',parts],['docx',unclosed],['docx',doctype]];
function child(format,bytes,budget){const run=spawnSync(executable,['--document-worker',format,String(budget)],{input:bytes,windowsHide:true,timeout:30000,maxBuffer:8*1024*1024});assert.equal(run.status,0,String(run.error??run.stderr));assert.equal(run.stderr.length,0,'Private child diagnostics must not expose the document');return JSON.parse(run.stdout.toString());}
for(const[format,input]of fixtures){const bytes=Buffer.isBuffer(input)?input:Buffer.from(input);let expected;try{expected={text:await extractDocument(format,bytes,6291456)};}catch(error){expected={code:/^media_[a-z_]+$/.test(error.code)?error.code:'media_document_invalid'};}const actual=child(format,bytes,6291456);try{assert.deepEqual(actual,expected);cases.push({format,hash:hashJson(actual)});}catch{failures.push({format,expected,actual});}}
for(const[format,budget,expected]of [['html',1,'media_native_input_too_large'],['html',0,'media_document_invalid'],['html',6291457,'media_document_invalid'],['pdf',6291456,'media_document_invalid'],['unknown',6291456,'media_document_invalid']]){assert.deepEqual(child(format,Buffer.from('<p>inert</p>'),budget),{code:expected});cases.push({format,budget,code:expected});}
const large=child('html',Buffer.alloc(32*1024*1024+1,120),6291456);assert.deepEqual(large,{code:'media_document_too_large'});cases.push({format:'html',code:large.code});
console.log(JSON.stringify({cases:cases.length,failures,receipts:cases,fixtureHash:digest(readFileSync(fixture)),generator:saved.generator}));if(failures.length)process.exitCode=1;
