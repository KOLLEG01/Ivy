// The original launch path rejects the deliberately relative proxy socket after
// executable verification and before shell/version/daemon/process invocation.
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { startCodexProcess } from '../../dist/packages/host-runtime/src/codex-process.js';
import { nativeCatalogPath } from '../../dist/packages/sdk/src/node.js';
import { IvyError } from '../../dist/packages/contracts/src/errors.js';
let text = '';
for await (const chunk of process.stdin) text += chunk;
const { root } = JSON.parse(text), rows = [];
const original = JSON.parse(await readFile('specs/native/codex-0.154.0/catalog.json', 'utf8'));
const bytes = Buffer.from('protocol-only executable identity fixture');
const hash = 'sha256:' + createHash('sha256').update(bytes).digest('hex');
for (const [mode, name] of [['match','fixture.exe'], ['hash','fixture.exe'], ['cmd','fixture.CMD'], ['hidden-cmd','.CMD'], ['bat','fixture.bat'], ['ps1','fixture.Ps1'], ['other-extension','fixture.txt'], ['directory','fixture.exe'], ['missing','fixture.exe']]) {
  const dir = join(root, mode), artifact = join(dir, 'artifact');
  const expectedHash = mode === 'hash' ? 'sha256:' + 'f'.repeat(64) : hash;
  const catalog = { ...original, nativeExecutableHash: expectedHash, clientRequests: original.clientRequests.filter(r => r.method === 'thread/realtime/start'), serverRequests: [], serverNotifications: [], clientNotifications: [], derivedLegacyTypes: [] };
  const file = nativeCatalogPath(join(artifact, 'dist'), catalog.version);
  await mkdir(join(file, '..'), { recursive:true }); await writeFile(file, JSON.stringify(catalog));
  const executable = join(dir, name);
  if (mode === 'directory') await mkdir(executable); else if (mode !== 'missing') await writeFile(executable, bytes);
  let expected;
  try {
    await startCodexProcess({ settings:{ nativeExecutable:executable, nativeExecutableHash:expectedHash, nativeVersion:catalog.version, codexHome:join(dir,'home'), appServer:{mode:'external-proxy',socketPath:'relative'} }, artifactRoot:artifact, dataRoot:join(dir,'data'), clientName:'Fixture',clientVersion:'1.0.0',onRequest(){throw Error('Unexpected native request');},onNotification(){throw Error('Unexpected notification');},onClose(){} });
    throw Error('Fixture must never start a native process');
  } catch (error) {
    expected = error.code === 'invalid_arguments' && error.message === 'Codex proxy requires an absolute local control socket.' ? {ok:true} : {ok:false,error:IvyError.from(error).toWire()};
  }
  rows.push({mode,executable,expectedHash,expected});
}
assert.equal(rows.find(r=>r.mode==='match').expected.ok,true);
assert.equal(rows.find(r=>r.mode==='other-extension').expected.ok,true);
for(const mode of ['hash','cmd','hidden-cmd','bat','ps1','directory'])assert.equal(rows.find(r=>r.mode===mode).expected.error.data.code,'native_build_mismatch');
const versions=[];
const check=(stdout,exitCode=0)=>versions.push({stdout,exitCode,expected:exitCode===0&&stdout.trim()==='codex-cli 0.154.0'});
check('codex-cli 0.154.0');check('codex-cli 0.154.1');check('codex-cli 0.154.0',1);check('codex-cli 0.154.0',null);
for(const unit of [9,10,11,12,13,32,0x85,0xa0,0x1680,0x180e,0x2000,0x2001,0x2002,0x2003,0x2004,0x2005,0x2006,0x2007,0x2008,0x2009,0x200a,0x200b,0x2028,0x2029,0x202f,0x205f,0x3000,0xfeff,0x10009]) {
  const c=String.fromCodePoint(unit);check(c+'codex-cli 0.154.0'+c);
}
for(const value of ['','\ufeff \n','codex-cli\t0.154.0','codex-cli  0.154.0','codex-cli 0.154.0 extra','codex-cli 0.154.0\0','codex-cli 0.154.0\nother'])check(value);
process.stdout.write(JSON.stringify({rows,versions}));
