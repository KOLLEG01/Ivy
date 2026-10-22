const quote=value=>'"'+value.replaceAll('"','""')+'"';
export function snapshot(db){
 const schema=db.prepare('SELECT type,name,tbl_name AS tableName,sql FROM sqlite_master ORDER BY type,name').all();
 const tables=schema.filter(row=>row.type==='table').map(row=>{
  const columns=db.prepare('PRAGMA table_info('+quote(row.name)+')').all().map(column=>column.name);
  const selected=['rowid',...columns].flatMap((column,i)=>['typeof('+quote(column)+') AS t'+i,'hex(CAST('+quote(column)+' AS BLOB)) AS b'+i]);
  const data=db.prepare('SELECT '+selected.join(',')+' FROM '+quote(row.name)+' ORDER BY rowid').all();
  return{name:row.name,columns,rows:data.map(value=>['rowid',...columns].flatMap((_,i)=>[value['t'+i],value['b'+i]]))};
 });
 return{schema,tables};
}
export function omitImportReceipts(value){
 const copy=structuredClone(value),table=copy.tables.find(t=>t.name==='journal_state');
 table.rows=table.rows.filter(row=>!Buffer.from(row[3],'hex').toString('utf8').startsWith('secretary/import/'));
 return copy;
}
export function seedRetainedValues(store){
 store.db.exec("PRAGMA user_version=37;CREATE TABLE retained_unknown(text_value TEXT,integer_value INTEGER,real_value REAL,blob_value BLOB);INSERT INTO retained_unknown VALUES('unrelated',42,0.25,x'00ff00d800');");
 store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run('retained-document','retained/unknown','retained',2,100,null,'{ "value": "\\ud800", "revision": 2 }');
 store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run('retained-document',1,'{ "value": "\\udc00", "revision": 1 }');
 store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run('retained-document',2,'{ "value": "\\ud800", "revision": 2 }');
 store.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run('retained-blob','retained/bytes','name',Buffer.from([0,255,0,216,0]));
 store.db.prepare('INSERT INTO technical_cleanup VALUES(?,?)').run('unrelated-root',1800000000000);
 store.db.prepare('INSERT INTO journal_state VALUES(?,?)').run('retained-unknown','{ "raw":"\\ud800" }');
 store.db.prepare('INSERT INTO retired_operations VALUES(?,?)').run('retained:1800000000000:operation',1800000000000);
}
export function seedTechnicalHistory(store,key,name,value,id='legacy-import'){
 const encoded=JSON.stringify(value);
 store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(id,key,name,3,encoded);
 for(let revision=1;revision<=3;revision++)store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(id,revision,revision===3?encoded:JSON.stringify({retainedHistory:revision,value:'\ud800',operationId:'retained-original-'+revision}));
}
