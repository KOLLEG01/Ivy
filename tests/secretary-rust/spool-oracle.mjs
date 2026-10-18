import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { createRequire } from 'node:module';
import { schemaSamples } from '../fixtures/schema-samples.mjs';
const build = resolve(process.env.IVY_SECRETARY_ORACLE ?? 'dist');
const module = path => pathToFileURL(join(build, path)).href;
const { MediaSpool } = await import(module('services/secretary/src/media-spool.js'));
const { validateMedia } = await import(module('services/secretary/src/media-schema.js'));
const { IvyError } = await import(module('packages/sdk/src/node.js'));
// Reuse the existing synthetic inputs without rebuilding or changing the oracle.
const fixtureSource = (await readFile('tests/secretary-media-fixture.mjs', 'utf8'))
  .replaceAll("'../dist/packages/sdk/src/node.js'", JSON.stringify(module('packages/sdk/src/node.js')))
  .replaceAll("'../dist/services/secretary/src/media-spool.js'", JSON.stringify(module('services/secretary/src/media-spool.js')));
const { owner, intent, materialization } = await import('data:text/javascript;base64,' + Buffer.from(fixtureSource).toString('base64'));
const [mode, root] = process.argv.slice(2);
if (mode === 'validation-cases') {
  const { Ajv2020 } = createRequire(import.meta.url)('ajv/dist/2020.js');
  const schema = JSON.parse(await readFile('specs/schemas/secretary-media.schema.json', 'utf8'));
  const document = { ...schema, $id: 'https://fixture.invalid/media' };
  const ajv = new Ajv2020({ strict:false, validateFormats:false, ownProperties:true });
  ajv.addSchema(document);
  const names = new Set(['MediaAttachment','MediaMaterialization','MediaDownloadEvidence','MediaAdmission','MediaIntent','MediaManifest','MediaReadPlan','MediaReadCall','MediaCoverage']);
  const rows = [];
  const add = (name, value) => {
    const input = JSON.stringify(value); let error = null;
    try { validateMedia(name, JSON.parse(input)); }
    catch (cause) { const e = IvyError.from(cause); error = { code:e.code, message:e.message, outcome:e.outcome }; }
    rows.push({ name, input, error });
  };
  const bases = new Map();
  for (const row of schemaSamples(ajv, [document])) {
    const name = row.reference.split('#/$defs/')[1];
    if (!names.has(name)) continue;
    const value = JSON.parse(row.input); add(name, value);
    if (row.valid) bases.set(name, value);
  }
  const stamp = '2026-10-18T00:00:00.000Z', hash = 'sha256:'+'a'.repeat(64);
  const evidence = { schemaVersion:1, attachment:materialization.attachment, fileId:materialization.fileId,
    materializationHash:intent.materializationHash, downloadUrlHash:hash, startedAt:stamp, observedAt:stamp,
    byteLength:15, contentHash:hash, providerSizeRelation:'equal',
    http:{status:200,contentLength:15,contentEncoding:null,contentType:null,contentMd5:null} };
  bases.set('MediaAttachment', materialization.attachment); bases.set('MediaMaterialization', materialization);
  bases.set('MediaAdmission', intent.admission); bases.set('MediaIntent', intent); bases.set('MediaDownloadEvidence', evidence);
  bases.set('MediaManifest', {schemaVersion:1,admission:intent.admission,evidence,chunks:[intent.item]});
  bases.set('MediaCoverage', {schemaVersion:1,admission:intent.admission,observedAt:stamp,gap:null,
    attachments:[{attachment:materialization.attachment,manifest:intent.item,errorCode:null}]});
  const stamps = [null,stamp,'0000-02-29T00:00:00.000Z','1900-02-29T00:00:00.000Z','2000-02-29T00:00:00.000Z',
    '2026-02-30T00:00:00Z','2026-01-01T24:00:00Z','2026-01-01T00:00:60Z','2026-01-01T00:00:00.1234567Z',
    '2026-01-01T00:00:00.12345678Z','2026-01-01T00:00:00Z','2026-01-01T00:00:00.1Z',
    '+010000-01-01T00:00:00.000Z','-000001-01-01T00:00:00.000Z','+001000-01-01T00:00:00.000Z',
    '-000000-01-01T00:00:00.000Z','+275760-09-13T00:00:00.000Z','+275760-09-13T00:00:00.001Z',
    '-271821-04-20T00:00:00.000Z','-271821-04-19T23:59:59.999Z','\ud800'];
  for (const [name, base] of bases) {
    add(name,base);
    for (const key of Object.keys(base)) {
      const omitted=structuredClone(base); delete omitted[key]; add(name,omitted);
      for (const replacement of [null,{},[],false,0,'\ud800']) add(name,{...base,[key]:replacement});
    }
    const paths = name==='MediaAttachment' ? [['modifiedAt']] : name==='MediaReadPlan' ? [['createdAt']]
      : name==='MediaReadCall' ? [['preparedAt']] : name==='MediaCoverage' ? [['observedAt']]
      : name==='MediaDownloadEvidence' ? [['attachment','modifiedAt'],['startedAt'],['observedAt']]
      : name==='MediaMaterialization' ? [['attachment','modifiedAt']] : [];
    for (const path of paths) for (const stamp of stamps) {
      const changed=structuredClone(base); let target=changed; for(const key of path.slice(0,-1))target=target[key];
      target[path.at(-1)]=stamp; add(name,changed);
    }
  }
  for (const encoding of [null,'identity','IDENTITY','IdEnTiTy','gzip',' identity','\ud800','İDENTITY'])
    add('MediaDownloadEvidence',{...evidence,http:{...evidence.http,contentEncoding:encoding}});
  add('MediaDownloadEvidence',{...evidence,providerSizeRelation:'different'});
  add('MediaDownloadEvidence',{...evidence,http:{...evidence.http,contentLength:14}});
  const coverage=bases.get('MediaCoverage');
  for (const gap of [null,'gap']) for (const manifest of [null,intent.item]) for (const errorCode of [null,'missing'])
    add('MediaCoverage',{...coverage,gap,attachments:[{attachment:materialization.attachment,manifest,errorCode}]});
  add('MediaCoverage',{...coverage,attachments:[...coverage.attachments,...coverage.attachments]});
  const call=bases.get('MediaReadCall'); assert.ok(call);
  const empty={...call,params:{padding:''}}, size=Buffer.byteLength(JSON.stringify(empty));
  for(const delta of [0,1]) add('MediaReadCall',{...empty,params:{padding:'x'.repeat(1024*1024-size+delta)}});
  assert.equal(bases.size,9);
  console.log(JSON.stringify(rows));
}
else if (mode === 'fixtures') console.log(JSON.stringify({ owner, intent, materialization }));
else if (mode === 'validate') {
  let input = ''; for await (const chunk of process.stdin) input += chunk;
  const { name, value } = JSON.parse(input); validateMedia(name, value);
} else {
  const spool = await MediaSpool.open(root, owner);
  try {
    if (mode === 'create') {
      spool.prepare(intent, materialization);
      await spool.acquire(intent.operationId, new AbortController().signal, async () => {}, async () => new Response(Buffer.from([0, 1, 254, 255])));
      spool.prepare({ ...intent, operationId: 'interrupted' }, materialization);
    } else if (mode === 'verify') {
      assert.deepEqual(spool.payload(intent.operationId).bytes, Buffer.from([0, 1, 254, 255]));
      assert.equal(spool.get('interrupted').errorCode, 'media_download_interrupted');
      assert.equal(spool.get('rust-original').phase, 'failed');
      assert.equal(spool.get('rust-original').errorCode, 'media_download_cancelled');
    } else throw Error('Unknown mode');
  } finally { spool.close(); }
  if (mode === 'create') {
    const db = new DatabaseSync(join(root, 'media.sqlite'));
    db.prepare("UPDATE attempts SET phase='downloading' WHERE operation_id='interrupted'").run();
    db.close();
  }
}
