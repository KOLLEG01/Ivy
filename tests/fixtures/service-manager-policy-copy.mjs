import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';

const input = JSON.parse(readFileSync(0,'utf8'));
const database = new DatabaseSync(input.database,{readOnly:input.mode!=='checkpoint'});
try {
  if (input.mode==='checkpoint') database.exec('PRAGMA wal_checkpoint(TRUNCATE)');
  const tables = database.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").all();
  const rows = Object.fromEntries(tables.map(({name})=>{
    const table = '"'+name.replaceAll('"','""')+'"';
    const filter = name==='meta' ? " WHERE key NOT GLOB 'runtime-policy:*' AND key NOT GLOB 'runtime-policy-import:*'" : '';
    return [name,database.prepare(`SELECT rowid,* FROM ${table}${filter} ORDER BY rowid`).all()];
  }));
  const files = input.files.map(path=>[path,createHash('sha256').update(readFileSync(path)).digest('hex')]);
  process.stdout.write(JSON.stringify({rows,files}));
} finally {database.close();}
