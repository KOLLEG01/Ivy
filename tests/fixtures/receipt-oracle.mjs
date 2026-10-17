import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
import assert from 'node:assert/strict';
registerHooks({resolve(specifier,context,next){
  if(specifier.endsWith('.js')&&specifier.startsWith('.')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }return next(specifier,context);
}});
const {ReceiptArchive}=await import('../../packages/sdk/src/receipt-archive.ts');
const [mode,path]=process.argv.slice(2);
const db=new DatabaseSync(path);const archive=new ReceiptArchive(db,{count:1,bytes:1000});
if(mode==='seed'){
  archive.retain('operation','js','{"value":"😀","phase":"done"}');
  archive.retain('operation','unknown','{"owner":{"outcome":"unknown"}}');
}else{
  assert.throws(()=>archive.read('operation','js'),{code:'operation_expired'});
  assert.equal(archive.read('operation','rust'),'original Rust receipt');
  assert.throws(()=>archive.release('operation','unknown'),{code:'receipt_archive_unresolved'});
  assert.equal(archive.status().records,2);
}db.close();
