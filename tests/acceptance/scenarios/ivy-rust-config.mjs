import assert from 'node:assert/strict';
import { readFile, realpath } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { randomBytes } from 'node:crypto';
import { coreConfiguration, assertLaneIsolation, validateEmittedSettings } from '../fixtures/ivy-rust-lanes.mjs';

const { values } = parseArgs({ options: Object.fromEntries(['distribution','settings','output'].map(key => [key,{type:'string'}])) });
for (const key of ['distribution','settings','output']) assert.ok(values[key], `Missing --${key}`);
const distribution = await realpath(resolve(values.distribution));
const load = name => import(pathToFileURL(join(distribution,'dist',name)).href);
const { validateHost } = await load('packages/contracts/src/validation.js');
const { hashJson } = await load('packages/contracts/src/canonical.js');
const { atomicJson } = await load('packages/host-runtime/src/config.js');
const { privateDirectory } = await load('packages/host-runtime/src/backup-files.js');
const { installationIdentity } = await load('packages/host-runtime/src/linux-resources.js');
const settings = JSON.parse(await readFile(resolve(values.settings),'utf8'));
const lanes = ['reference','candidate'].map(name => {
  const credential = randomBytes(32).toString('hex');
  const lane = { name };
  for (const [target,os] of [['linux','linux'],['windows','win32']]) {
    lane[target] = coreConfiguration({...settings[name][target],os},credential);
    validateEmittedSettings(lane[target],validateHost);
  }
  return lane;
});
assertLaneIsolation(lanes);
// This creates transportable private configs, not installation roots on either target.
const output = resolve(values.output);
await privateDirectory(output); // Creates a fresh private directory; never adopts existing output.
const receipt = { schemaVersion:1, servicesStarted:false, resourcesCreated:[], configurations:[] };
for (const lane of lanes) for (const target of ['linux','windows']) {
  const config = lane[target];
  const filename = `${lane.name}-${target}.json`;
  await atomicJson(join(output,filename),config);
  receipt.configurations.push({ lane:lane.name,target,filename,hostId:config.hostId,configHash:hashJson(config),installationId:installationIdentity(config) });
}
await atomicJson(join(output,'receipt.json'),receipt);
console.log(JSON.stringify(receipt));
