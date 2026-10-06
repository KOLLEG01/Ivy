import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { bundleLocalSchema, hashJson, SchemaValidators } from '../dist/packages/sdk/src/node.js';
import { bundled as secretaryBundle } from '../dist/services/secretary/src/schema.js';

test('Secretary contract hashes are independent of unrelated Settings changes', () => {
  for (const [file, bundle, definition] of [['secretary', secretaryBundle, 'Item']]) {
    const schema = JSON.parse(readFileSync(`specs/schemas/${file}.schema.json`, 'utf8')), original = bundle(definition);
    assert.equal(hashJson(original), hashJson(bundleLocalSchema(schema, definition)));
    schema.$defs.Settings.properties.unrelatedOptionalSetting = { type: 'string' };
    assert.equal(hashJson(original), hashJson(bundleLocalSchema(schema, definition)));
    assert.equal(Object.hasOwn(original.$defs, 'Settings'), false);
    new SchemaValidators().compile(original);
  }
});
