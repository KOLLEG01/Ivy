import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { verifyTransferredImage, verifyImageRelocation } from '../scenarios/receive-prepared.mjs';

test('transferred Hive image must match the explicit immutable identity, build and platform', () => {
  const id = 'sha256:' + 'a'.repeat(64), other = 'sha256:' + 'b'.repeat(64);
  const candidate = { componentId: 'hive', dockerImage: id, buildId: other, platform: { os: 'linux', arch: 'x64' } };
  const image = { Id: id, Os: 'linux', Architecture: 'amd64', Config: { Labels: { 'dev.ivy.build': other } } };
  assert.doesNotThrow(() => verifyTransferredImage([image], candidate, id));
  for (const altered of [{ ...image, Id: other }, { ...image, Architecture: 'arm64' }, { ...image, Os: 'windows' }, { ...image, Config: { Labels: { 'dev.ivy.build': id } } }]) {
    assert.throws(() => verifyTransferredImage([altered], candidate, id));
  }
  for (const expected of [undefined, 'ivy/hive:latest', other]) assert.throws(() => verifyTransferredImage([image], candidate, expected));
  assert.throws(() => verifyTransferredImage([], candidate, id));
  assert.throws(() => verifyTransferredImage([image, image], candidate, id));
  assert.throws(() => verifyTransferredImage([image], { ...candidate, componentId: 'agent-manager' }, id));
});

test('OCI index to local config identity follows verified blobs and refuses tampering or an unrelated image', async () => {
  const blobs = new Map();
  const put = object => { const bytes = Buffer.from(JSON.stringify(object)), id = 'sha256:' + createHash('sha256').update(bytes).digest('hex'); blobs.set(id, bytes); return id; };
  const buildId = 'sha256:' + 'b'.repeat(64);
  const configId = put({ os: 'linux', architecture: 'amd64', config: { Labels: { 'dev.ivy.build': buildId } } });
  const manifestId = put({ mediaType: 'application/vnd.oci.image.manifest.v1+json', config: { digest: configId } });
  const descriptor = { platform: { os: 'linux', architecture: 'amd64' }, digest: manifestId };
  const indexId = put({ mediaType: 'application/vnd.oci.image.index.v1+json', manifests: [descriptor] });
  const candidate = { dockerImage: indexId, buildId, platform: { os: 'linux', arch: 'x64' } };
  const read = async id => blobs.get(id);
  await verifyImageRelocation(candidate, configId, read);
  await verifyImageRelocation({ ...candidate, dockerImage: manifestId }, configId, read);
  await assert.rejects(verifyImageRelocation(candidate, buildId, read));
  await assert.rejects(verifyImageRelocation({ ...candidate, buildId: configId }, configId, read));
  await assert.rejects(verifyImageRelocation({ ...candidate, platform: { os: 'linux', arch: 'arm64' } }, configId, read));
  const duplicate = put({ mediaType: 'application/vnd.oci.image.index.v1+json', manifests: [descriptor, descriptor] });
  await assert.rejects(verifyImageRelocation({ ...candidate, dockerImage: duplicate }, configId, read));
  blobs.set(manifestId, Buffer.from('{}'));
  await assert.rejects(verifyImageRelocation(candidate, configId, read));
});
