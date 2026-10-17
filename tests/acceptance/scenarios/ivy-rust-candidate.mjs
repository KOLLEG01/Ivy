import assert from 'node:assert/strict';
import { readFile, realpath } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { qualifyPreparedCandidate } from '../../../tools/testing/ivy-rust-differential.mjs';

// Read-only target-side admission. Installation/activation is a separate CLI operation.
const keys = ['distribution','config','candidate','expected-id'];
const { values } = parseArgs({ options:Object.fromEntries(keys.map(key=>[key,{type:'string'}])) });
for (const key of keys) assert.ok(values[key],`Missing --${key}`);
const distribution = await realpath(resolve(values.distribution));
const load = name=>import(pathToFileURL(join(distribution,'dist',name)).href);
const { verifyCandidate } = await load('packages/host-runtime/src/artifact.js');
const { validateHost } = await load('packages/contracts/src/validation.js');
const candidate = JSON.parse(await readFile(resolve(values.candidate),'utf8'));
validateHost('Candidate',candidate);
assert.equal(candidate.candidateId,values['expected-id']);
qualifyPreparedCandidate(candidate,{os:process.platform,arch:process.arch,node:process.version});
const config = JSON.parse(await readFile(resolve(values.config),'utf8'));
validateHost('HostConfigInput',config);
assert.ok(config.artifactRoot, 'An explicit existing artifact root is required');
await realpath(config.artifactRoot);
await verifyCandidate(candidate,config);
console.log(JSON.stringify({candidateId:candidate.candidateId,componentId:candidate.componentId,platform:candidate.platform,admitted:true,activated:false}));
