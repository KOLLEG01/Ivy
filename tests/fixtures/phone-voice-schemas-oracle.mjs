import { createRequire, registerHooks } from 'node:module';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
const require = createRequire(import.meta.url);
const external = new Map(['ajv/dist/2020.js', 're2js'].map(name => [name, pathToFileURL(require.resolve(name)).href]));
registerHooks({ resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(external.get(specifier) ?? specifier, context);
} });
const { SchemaValidators } = await import('../../packages/contracts/src/schema.ts');
const catalog = JSON.parse(readFileSync(new URL('../../specs/native/codex-0.154.0/catalog.json', import.meta.url), 'utf8'));
const cases = JSON.parse(readFileSync(new URL('./phone-voice-schema-cases.json', import.meta.url), 'utf8'));
const validators = new SchemaValidators();
const rows = [];
for (const item of cases) {
  const schema = Object.hasOwn(item, 'schema') ? item.schema : catalog.clientRequests.find(definition => definition.method === item.method)[item.direction];
  const maximum = item.maximumBytes ?? (item.direction === 'inputSchema' ? 6 * 1024 * 1024 : item.method === 'model/list' ? 256 * 1024 : 64 * 1024);
  let row;
  try { validators.validate(schema, item.value, maximum); row = { ok: true }; }
  catch (error) { row = { ok: false, code: error.code, message: error.message }; }
  rows.push({ name: item.name, ...row });
}
process.stdout.write(JSON.stringify(rows));
