import {environmentName,readEnvironmentDocument,selectEnvironmentDocument} from '../../dist/services/agent-manager/src/environment.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
import assert from 'node:assert/strict';
const document=(kind,hostId,enabled)=>({schemaVersion:1,hostId,enabled,[kind==='mcp'?'servers':'files']:[]});
const source=(kind,hostId,mode)=>mode==='absent'?null:(()=>{const d=document(kind,hostId,mode);return {document:d,objectId:'object-'+(hostId??'global'),revision:4,contentHash:hashJson(d)};})();
const names=[],selections=[],reads=[];
for(const kind of ['mcp','skills']){
  for(const hostId of [null,'','host','Host','host 😀','\ud800','\ufffd','\udc00','a\0b','e\u0301','é'])names.push({kind,hostId,expected:environmentName(kind,hostId)});
  for(const enabled of [true,false])for(const g of ['absent',null,false,true])for(const h of ['absent',null,false,true]){
    const defaults=document(kind,null,enabled),global=source(kind,null,g),host=source(kind,'host',h);
    selections.push({kind,defaults,global,host,expected:selectEnvironmentDocument(defaults,global,host)});
    if(g===null&&h===null){
      const changed={...global,revision:5};
      const expected=selectEnvironmentDocument(defaults,changed,host);
      assert.deepEqual(expected.document,defaults);
      assert.notEqual(expected.version,selections.at(-1).expected.version);
      selections.push({kind,defaults,global:changed,host,expected});
    }
  }
  for(const hostId of [null,'host'])for(const mode of ['valid','missing','archived','null-stat','stat-error','read-missing','read-unknown','contract','version','encoding','scope','schema','null-enabled']){
    const stat={id:'object',currentRevision:7,effectivelyArchived:mode==='archived'};
    const value={object:{id:'read-object',contractKey:kind==='mcp'?'agent/mcp-configuration':'agent/skills'},revision:{revision:99,contractVersion:'1.0.0',contentHash:'sha256:'+'a'.repeat(64)},content:{encoding:'json',value:document(kind,hostId,mode==='null-enabled'?null:true)}};
    if(mode==='contract')value.object.contractKey='other';
    if(mode==='version')value.revision.contractVersion='2.0.0';
    if(mode==='encoding')value.content.encoding='base64';
    if(mode==='scope')value.content.value.hostId=hostId===null?'host':null;
    if(mode==='schema')value.content.value.enabled='true';
    const statError=mode==='missing'?{code:'not_found',outcome:'not_executed'}:mode==='stat-error'?{code:'unavailable',outcome:'unknown'}:null;
    const readError=mode==='read-missing'?{code:'not_found',outcome:'not_executed'}:mode==='read-unknown'?{code:'outcome_unknown',outcome:'unknown'}:null;
    const input={kind,hostId,mode,stat:mode==='null-stat'?null:stat,value,statError,readError},calls=[];
    const connection={async request(method,params){calls.push({method,params});const error=method==='objects.stat'?statError:readError;if(error)throw new IvyError(error.code,'fixture failure',{outcome:error.outcome});return method==='objects.stat'?input.stat:value;}};
    let expected;
    try{expected={result:await readEnvironmentDocument(connection,kind,hostId,kind==='mcp'?'McpDocument':'SkillsDocument')};}
    catch(error){expected={error:IvyError.from(error).toWire()};}
    reads.push({...input,expected,calls});
    assert.equal(calls.length,['missing','archived','null-stat','stat-error'].includes(mode)?1:2);
    if(mode==='read-missing')assert.equal(expected.error.data.code,'not_found');
    if(mode==='valid')assert.equal(expected.result.revision,7);
  }
}
process.stdout.write(JSON.stringify({names,selections,reads}));
