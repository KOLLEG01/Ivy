import {createRequire, registerHooks} from 'node:module';
import {pathToFileURL} from 'node:url';

// ESM oracles use the same installed dependencies as CommonJS, including the
// caller's NODE_PATH when the worktree shares its existing test dependencies.
const require = createRequire(import.meta.url);
const external = new Map([
  'ajv/dist/2020.js',
  'ajv/dist/compile/index.js',
  'ajv/dist/compile/resolve.js',
  'ajv/dist/compile/validate/subschema.js',
  'fast-uri',
  'json-schema-traverse',
  're2js',
].map(name => [name, pathToFileURL(require.resolve(name)).href]));

registerHooks({resolve(specifier, context, next) {
  if (context.conditions.includes('require')) return next(specifier, context);
  return next(external.get(specifier) ?? specifier, context);
}});
