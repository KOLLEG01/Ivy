// Load the actual TS store as an isolated migration oracle. Only its SDK import
// is supplied locally, avoiding a whole backend build for this storage fixture.
import { readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { join, dirname } from 'node:path';
import { stripTypeScriptTypes } from 'node:module';
import assert from 'node:assert/strict';

const [phase, database] = process.argv.slice(2);
const source = readFileSync(new URL('../../services/data-collector/src/store.ts', import.meta.url), 'utf8');
const transformed = stripTypeScriptTypes(source, { mode: 'transform' }).replace(
  /import\s*\{\s*digest,\s*requireThat\s*\}\s*from\s*"[^"]+";/,
  `import {createHash} from 'node:crypto';
   const digest = text => 'sha256:' + createHash('sha256').update(text).digest('hex');
   const requireThat = (condition,code,message) => {if(!condition)throw Object.assign(new Error(message),{code});};`,
);
assert(!transformed.includes('packages/sdk/'), 'Oracle SDK substitution must match exactly');
const modulePath = join(dirname(database), 'collector-store-oracle.mjs');
writeFileSync(modulePath, transformed);
const { CollectorStore } = await import(pathToFileURL(modulePath));
const store = new CollectorStore(database);
// Both shapes succeed in the unchanged Node store, but exceed the former
// ordinary Rust parser's validator depth/node caps.
const opaque = {value:'\ud800',deep:JSON.parse('['.repeat(300)+'false'+']'.repeat(300)),wide:Array(400001).fill(0)};
try {
  if (phase === 'seed') {
    store.save({task:{id:'oracle',enabled:false,intervalSeconds:5,script:'original',config:{},dependencies:{}},revision:4,resultObjectId:'result',nextAt:5000,state:{cursor:0},deleted:false,lastRunId:null,retentionWarning:null,future:opaque});
    store.saveRun({id:'queued',taskId:'oracle',task:store.get('oracle').task,status:'queued',input:false,output:null,startedAt:null,finishedAt:null,eventIndex:0,error:null});
    store.saveRun({id:'pending',taskId:'oracle',task:store.get('oracle').task,status:'publishing',input:0,output:{data:false,state:0,events:[]},startedAt:'2026-01-01T00:00:00.000Z',finishedAt:'2026-01-01T00:00:01.000Z',eventIndex:0,error:null});
    store.mutate('receipt','request',()=>({runId:'pending',value:0}));
    store.setMetadata('definition-write:prepared:step','{"exact":"bytes"}');
    store.db.prepare('INSERT INTO retired_operations VALUES(?,?)').run('retired','sha256:retained');
    store.db.prepare('INSERT INTO retired_runs VALUES(?)').run('retired-run');
  } else if (phase === 'verify') {
    assert.equal(store.get('oracle').future.value,'\ud800');
    assert.equal(JSON.stringify(store.get('oracle').future),JSON.stringify(opaque));
    assert.equal(store.get('oracle').state,null);
    assert.equal(store.get('oracle').revision,4);
    assert.equal(store.run('pending').executionCommitted,true);
    assert.equal(store.run('pending').output.state,0);
    assert.deepEqual(store.operation('receipt'),{runId:'pending',value:0});
    assert.equal(store.operation('rust-receipt').saved,true);
    assert.equal(JSON.stringify(store.operation('rust-receipt').future),JSON.stringify(opaque));
    assert.equal(store.metadata('definition-write:prepared:step'),'{"exact":"bytes"}');
    assert.equal(store.usedRunId('retired-run'),true);
    assert.throws(()=>store.operation('retired'),{code:'operation_expired'});
    assert.equal(store.db.prepare('PRAGMA user_version').get().user_version,0);
    store.mutate('node-after-rust','request2',()=>({saved:true,future:store.operation('rust-receipt').future}));
  } else throw new Error('Unknown oracle phase');
} finally { store.close(); }
