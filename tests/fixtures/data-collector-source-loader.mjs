// Source-only reference services, including their storage worker threads.
import { createRequire, registerHooks } from 'node:module';
import { existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';

const require = createRequire(process.env.IVY_TEST_PACKAGE_JSON || import.meta.url);
const dependencies = new Map(
  ['ajv', 'ajv/dist/2020.js', 're2js', 'zod', 'ws', 'web-push', 'yauzl', 'fflate',
    '@modelcontextprotocol/server', '@modelcontextprotocol/node'].map(name => {
    let target = pathToFileURL(require.resolve(name));
    if (name === 'ws') target = new URL('./wrapper.mjs', target);
    if (name.startsWith('@modelcontextprotocol/')) target = new URL(target.href.replace(/\.cjs$/, '.mjs'));
    return [name, target.href];
  }),
);
registerHooks({
  resolve(specifier, context, next) {
    if (dependencies.has(specifier)) return next(dependencies.get(specifier), context);
    if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
      const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
      if (existsSync(fileURLToPath(source))) return next(source.href, context);
    }
    return next(specifier, context);
  },
});
