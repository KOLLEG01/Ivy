// Shared inputs, not expected native behavior. Expected results come from the
// unchanged original functions against disposable format-4 databases.
export {default as observations} from './hive-reset-storage-observations.json' with {type:'json'};
const at='2026-01-02T03:04:05.006Z',open={op:'open'},sql=sql=>({op:'sql',sql}),capture={op:'capture'};
const root=(ids,extra={})=>({op:'roots',ids,at,...extra});
function saved(id='kept',changes={}){
 return {family:{key:'agent/instructions',owner:'{ "kind" : "hive" }',mediaType:'text/markdown',objectMode:'retain'},
 contract:{version:'1.2.3',definition:'{ "raw" : "definition", "line":"\\u0061" }'},
 object:{id,parentId:null,ownerObjectId:null,name:'Instruction '+id,path:'/Instruction '+id,position:7,icon:'🦉',depth:1,currentRevision:4,createdAt:'created raw',updatedAt:'updated raw'},
 revision:{contractVersion:'1.2.3',encoding:'text',content:'  # Résumé\r\nkeep\u0000raw 🦉  ',contentHash:'raw-content-hash',byteLength:31,createdAt:'revision raw',referencesJson:'{}'},...changes};
}
const q=value=>value===null?'NULL':typeof value==='number'?String(value):String(value).split('\u0000').map(part=>"'"+part.replaceAll("'","''")+"'").join('||char(0)||');
function seed(value,searchId=37,{archived=false,effective=archived,encoding=value.revision.encoding,content=value.revision.content}={}){
 const {family:f,contract:c,object:o,revision:r}=value;
 return sql(`BEGIN IMMEDIATE;
 INSERT OR IGNORE INTO contract_families VALUES(${[f.key,f.owner,f.mediaType,f.objectMode].map(q)});
 INSERT OR IGNORE INTO contracts VALUES(${[f.key,c.version,c.definition].map(q)});
 INSERT INTO objects VALUES(${[searchId,o.id,o.parentId,o.ownerObjectId,o.name,o.path,o.position,o.icon,o.depth,f.key,o.currentRevision,archived?'archive raw':null,effective?1:0,o.createdAt,o.updatedAt].map(q)});
 INSERT INTO revisions VALUES(${[o.id,o.currentRevision,f.key,r.contractVersion,encoding].map(q)},${encoding==='base64'?content:q(content)},${[r.contentHash,r.byteLength,r.createdAt,r.referencesJson].map(q)});
 INSERT INTO object_fts(rowid,object_id,name,text) VALUES(${[searchId,o.id,o.name,'old searchable text'].map(q)});COMMIT;`);
}
function service(key='service/explicit',extra={}){return {key,version:'1.0.0',owner:{kind:'service',serviceName:'example'},mediaType:'application/json',retention:{objects:{mode:'retain'},revisions:{mode:'current'}},specMarkdown:'Explicit reset bootstrap.',jsonSchema:{type:'object'},...extra};}
const rebuild=(values=[],extra={})=>({op:'rebootstrap',epoch:'epoch-selected',values,ids:['scope-b','scope-a','scope-a'],definitions:[service()],at,...extra});
export function cases(){
 const a=saved(),b=saved('second'),archived=saved('archived'),withRefs=saved('references');withRefs.revision.referencesJson='{ }';
 const parent=saved('parented');parent.object.parentId='missing';
 const owner=saved('owned');owner.object.ownerObjectId='missing';
 const broken=saved('broken');broken.contract.version='other';
 const many=saved('max-position');many.object.position=Number.MAX_SAFE_INTEGER;
 const sourceRows=[open,seed(a),seed(archived,38,{archived:true}),seed(saved('hidden'),39,{effective:true}),
  sql("INSERT INTO revisions VALUES('kept',1,'agent/instructions','1.2.3','text','old history','old hash',11,'old time','{}');INSERT INTO principals VALUES('principal');INSERT INTO service_nodes VALUES('node','principal','host','example',1,'{\"connected\":true,\"synced\":true,\"ready\":true,\"extra\":1}');INSERT INTO subscriptions VALUES('node','sub','{}',0,0,3,'[]');"),capture];
 return [
  {name:'missing-capture-does-not-create',steps:[capture]},
  {name:'raw-current-active-capture-and-startup',steps:sourceRows},
  {name:'capture-clear-restore-retains-only-current',steps:[...sourceRows,{op:'discard'},open,{op:'restore'},capture]},
  {name:'restore-exact-identities-and-bootstrap',steps:[open,{op:'restore',values:[a,b]},root(['scope-b','scope-a','scope-a','🙂','\ue000']),{op:'contracts',definitions:[service()]},capture]},
  {name:'restore-late-root-guard-rolls-back',steps:[open,{op:'restore',values:[a,parent]}]},
  {name:'restore-owner-guard-rolls-back',steps:[open,{op:'restore',values:[a,owner]}]},
  {name:'restore-literal-references-guard-rolls-back',steps:[open,{op:'restore',values:[a,withRefs]}]},
  {name:'restore-late-foreign-key-commit-rolls-back',steps:[open,sql("CREATE TRIGGER skip_fixture_revision BEFORE INSERT ON revisions WHEN NEW.object_id='broken' BEGIN SELECT RAISE(IGNORE); END;"),{op:'restore',values:[a,broken]}]},
  {name:'restore-duplicate-object-rolls-back',steps:[open,{op:'restore',values:[a,a]}]},
  {name:'restore-keeps-existing-ignored-contract',steps:[open,sql("INSERT INTO contract_families VALUES('agent/instructions','existing owner','text/plain','retain');INSERT INTO contracts VALUES('agent/instructions','1.2.3','existing definition');"),{op:'restore',values:[a]}]},
  {name:'restore-does-not-adjust-retention',steps:[open,seed(a),sql("INSERT INTO retention_families VALUES('agent/instructions','policy',1,'{\"objectCount\":7,\"revisionCount\":9,\"byteLength\":123}');"),{op:'restore',values:[b]}]},
  {name:'blob-capture-string-and-restore-refusal',steps:[open,seed(a,37,{encoding:'base64',content:"X'0001FF'"}),capture,{op:'discard'},{...rebuild(),values:undefined}]},
  {name:'empty-root-list-registers-contract',steps:[open,root([])]},
  {name:'invalid-root-list-precedes-registration',steps:[open,root(['valid','']),root(['x'.repeat(257)]),root(['🙂'.repeat(129)])]},
  {name:'root-utf16-length-sort-and-lossy-sql',steps:[open,root(['🙂'.repeat(128),'\ue000','🙂']),root([],{idUnits:[[0xd800],[0xdc00],[0xd800]]})]},
  {name:'root-conflict-rolls-back-objects-keeps-contract',steps:[open,{op:'restore',values:[a]},root(['a-root','kept'])]},
  {name:'root-position-overflow-keeps-contract',steps:[open,{op:'restore',values:[many]},root(['new'])]},
  {name:'existing-root-metadata-only-guard',steps:[open,root(['fixed']),sql("UPDATE objects SET effective_archive=1,name='retained',path='/retained' WHERE id='fixed';"),root(['fixed'])]},
  {name:'archived-root-conflict',steps:[open,root(['fixed']),sql("UPDATE objects SET archived_at='old' WHERE id='fixed';"),root(['fixed'])]},
  {name:'service-late-owner-guard-keeps-earlier',steps:[open,{op:'contracts',definitions:[service(),service('other',{owner:{kind:'hive'}})]}]},
  {name:'service-null-owner-precedes-validation',steps:[open,{op:'contracts',definitions:[service(),{owner:null}]}]},
  {name:'service-invalid-definition-keeps-earlier',steps:[open,{op:'contracts',definitions:[service(),{owner:{kind:'service',serviceName:'example'}}]}]},
  {name:'rebootstrap-success',steps:[rebuild([a,b]),capture]},
  {name:'rebootstrap-epoch-survives-restore-failure',steps:[rebuild([a,withRefs])]},
  {name:'rebootstrap-restore-survives-clock-failure',steps:[rebuild([a],{clockError:true})]},
  {name:'rebootstrap-restore-survives-root-failure',steps:[rebuild([a],{ids:['']})]},
  {name:'rebootstrap-roots-service-survive-late-failure',steps:[rebuild([a],{definitions:[service(),service('other',{owner:{kind:'hive'}})]})]},
  {name:'restore-null-object-is-internal',steps:[open,{op:'restore',values:[{object:null}]}]},
  {name:'restore-guard-short-circuits-missing-revision',steps:[open,{op:'restore',values:[{object:{parentId:'foreign'}}]}]},
 ];
}
