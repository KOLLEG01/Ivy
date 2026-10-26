// OS and SQLite fixture only. No application source or policy is loaded.
import {readFileSync,existsSync,statSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
import {setup} from './hive-ui-files-fixture.mjs';
const digest=bytes=>'sha256:'+createHash('sha256').update(bytes).digest('hex');
function inspect(path){
  if(!existsSync(path))return{exists:false};
  if(statSync(path).isDirectory())return{exists:true,directory:true};
  const bytes=readFileSync(path);
  let db;
  try{
    db=new DatabaseSync(path,{readOnly:true});
    const tables=new Set(db.prepare("SELECT name FROM sqlite_schema WHERE type='table'").all().map(row=>row.name));
    const result={exists:true,bytes:bytes.length,digest:digest(bytes),pages:db.prepare('PRAGMA page_count').get().page_count,check:db.prepare('PRAGMA quick_check').get().quick_check};
    if(tables.has('sentinel'))result.sentinel=db.prepare('SELECT value FROM sentinel').all();
    if(tables.has('fixture_payload')){
      const hash=createHash('sha256');let count=0;
      for(const row of db.prepare('SELECT id,bytes FROM fixture_payload ORDER BY id').iterate()){
        hash.update(String(row.id)+'\0');hash.update(row.bytes);count++;
      }
      result.payload={count,hash:'sha256:'+hash.digest('hex')};
    }
    if(tables.has('credentials'))result.credentials=db.prepare('SELECT principal_id,digest,revoked FROM credentials ORDER BY digest').all();
    if(tables.has('metadata'))result.probe=db.prepare("SELECT value FROM metadata WHERE key='probe'").get()?.value??null;
    return result;
  }catch{return{exists:true,bytes:bytes.length,digest:digest(bytes),invalid:true};}
  finally{db?.close();}
}
const input=JSON.parse(readFileSync(0,'utf8'));
let output;
if(input.inspect)output=input.inspect.map(inspect);
else if(input.files){setup(input.root,input.files);output=null;}
else{
 const db=new DatabaseSync(input.path,{readOnly:true});
 try{output=Object.fromEntries(Object.entries(input.queries).map(([key,sql])=>[key,db.prepare(sql).all().map(row=>Object.fromEntries(Object.entries(row).map(([name,value])=>[name,value instanceof Uint8Array?{blob:[...value]}:value])))]));}
 finally{db.close();}
}
process.stdout.write(JSON.stringify(output));