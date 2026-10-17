import assert from 'node:assert/strict';
import { mkdir, open, readFile, realpath } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { runDifferential } from '../../../tools/testing/ivy-rust-differential.mjs';

const keys = ['distribution','reference','candidate','configuration-receipt','corpus','evidence'];
const { values } = parseArgs({ options: Object.fromEntries(keys.map(key=>[key,{type:'string'}])) });
for (const key of keys) assert.ok(values[key],`Missing --${key}`);
const distribution = await realpath(resolve(values.distribution));
const load = name => import(pathToFileURL(join(distribution,'dist',name)).href);
const { HiveClient } = await load('packages/sdk/src/client.js');
const { privateDirectory } = await load('packages/host-runtime/src/backup-files.js');
const { hashJson } = await load('packages/contracts/src/canonical.js');
const { validateHost } = await load('packages/contracts/src/validation.js');
const read = async path => JSON.parse(await readFile(resolve(path),'utf8'));
const reference = await read(values.reference), candidate = await read(values.candidate);
const receipt = await read(values['configuration-receipt']);
assert.equal(receipt.schemaVersion,1);
for (const [lane,config] of [['reference',reference],['candidate',candidate]]) {
  const entry = receipt.configurations.find(x=>x.lane===lane && x.target==='linux');
  assert.ok(entry && entry.hostId===config.hostId && entry.configHash===hashJson(config), 'Use the exact freshly generated lab configuration receipt');
}
for (const config of [reference,candidate]) {
  validateHost('HostConfigInput',config);
  const url = new URL(config.publicBaseUrl);
  assert.equal(url.protocol,'https:');assert.equal(url.pathname,'/ivy');
  assert.ok(!url.username && !url.password && !url.search && !url.hash);
}
assert.notEqual(reference.hostId,candidate.hostId);
assert.notEqual(new URL(reference.publicBaseUrl).origin,new URL(candidate.publicBaseUrl).origin);
const credential = config => {
  const hive = config.instances.find(x=>x.componentId==='hive');
  assert.ok(hive && hive.credential === hive.settings.credentials[0].token);
  return hive.credential;
};
assert.notEqual(credential(reference),credential(candidate));
const corpus = await read(values.corpus);
assert.equal(corpus.schemaVersion,1);
assert.ok(corpus.sourceCommit && corpus.contractHash && corpus.steps.length);
// Deliberately require reviewed fixture endpoints, and do not infer a permission to invoke
// account-backed adapters from a package's presence in the service registry.
assert.deepEqual(corpus.origins,[new URL(reference.publicBaseUrl).origin,new URL(candidate.publicBaseUrl).origin]);
assert.equal(corpus.externalEffects,'local-fixtures-only');
const evidence = resolve(values.evidence);
await mkdir(evidence);
await privateDirectory(evidence);
const record = async(name,value) => {
  assert.match(name,/^[a-z0-9][a-z0-9-]*$/);
  const handle=await open(join(evidence,name+'.json'),'wx',0o600);
  try { await handle.writeFile(JSON.stringify(value,null,2)+'\n');await handle.sync(); } finally { await handle.close(); }
};
await record('inputs',{sourceCommit:corpus.sourceCommit,contractHash:corpus.contractHash,corpusHash:hashJson(corpus),referenceConfigHash:hashJson(reference),candidateConfigHash:hashJson(candidate),client:'public-js-sdk'});
const clients = [reference,candidate].map(config=>new HiveClient(config.publicBaseUrl,{credential:credential(config),batchReads:false}));
const results = await runDifferential({steps:corpus.steps,reference:clients[0],candidate:clients[1],record});
await record('result',{passed:true,results,qualification:'selected public JS client scenarios only'});
console.log(JSON.stringify({passed:true,steps:results.length}));
