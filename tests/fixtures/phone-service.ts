import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {readFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {randomUUID} from 'node:crypto';
import {atomicJson} from '../../packages/host-runtime/src/config.js';
import {until} from './host.js';

export const phoneArtifactRoot = () => resolve(process.env['IVY_TEST_PHONE_ARTIFACT'] ?? '.');

interface Health {
  ready: boolean;
  generation: number | null;
  instanceId: string;
  launchId: string;
  bootId: string;
}

/** Local health belongs to the native owner; Hive publication is checked by the caller. */
export async function startNativePhone(path: string) {
  const config = JSON.parse(await readFile(path, 'utf8')) as {artifactRoot:string;dataRoot:string};
  const launchId=randomUUID();
  const suffix=process.platform==='win32'?'.exe':'';
  const child = spawn(join(config.artifactRoot, `dist/native/${process.platform}-${process.arch}/ivy-phone-bridge${suffix}`), ['--config', path], {
    env: {...process.env, IVY_LAUNCH_ID:launchId}, windowsHide:true, stdio:['ignore','pipe','pipe'],
  });
  let diagnostic='';
  child.stdout.resume();
  child.stderr.on('data', bytes => {diagnostic=(diagnostic+String(bytes)).slice(-8192);});
  const completed = new Promise<number | null>((done,reject) => {
    child.once('error',reject);child.once('close',done);
  });
  void completed.catch(()=>undefined);
  const health = async () => JSON.parse(await readFile(join(config.dataRoot,'health.json'),'utf8')) as Health;
  let incarnation: Health | undefined;
  async function waitLocalReady(previousGeneration?: number | null) {
    await until(async () => {
      assert.equal(child.exitCode,null,diagnostic);
      let value: Health;
      try { value=await health(); } catch (error) {
        if ((error as NodeJS.ErrnoException).code==='ENOENT') return false;
        throw error;
      }
      if (!value.ready || value.launchId!==launchId || previousGeneration!==undefined && (value.generation??0)<=(previousGeneration??0)) return false;
      incarnation=value;return true;
    },30000);
    return incarnation!;
  }
  async function close() {
    if(child.exitCode!==null) {assert.equal(child.exitCode,0,diagnostic);return;}
    const value=incarnation??await waitLocalReady();
    await atomicJson(join(config.dataRoot,'control.json'), {schemaVersion:1,instanceId:value.instanceId,
      launchId:value.launchId,bootId:value.bootId,action:'shutdown',requestedAt:new Date().toISOString()});
    try {await until(()=>child.exitCode!==null,15000);} catch(error) {child.kill();await completed.catch(()=>undefined);throw error;}
    assert.equal(await completed,0,diagnostic);
    assert.equal((await health()).ready,false);
  }
  try {await waitLocalReady();} catch(error) {child.kill();await completed.catch(()=>undefined);throw error;}
  return {waitLocalReady,close,health};
}
