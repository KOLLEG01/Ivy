import { copyFileSync, existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

function commandPath(command) {
  const path = command?.args?.[0];
  if (command?.executable !== 'node' || typeof path !== 'string' || !/^dist\/[A-Za-z0-9_./-]+\.js$/.test(path) || path.split('/').some(part => part === '..' || part === '.')) {
    throw Error('Native dispatch requires the existing relative Node entrypoint path.');
  }
  return path.slice('dist/'.length);
}

/** Render the exact approved v1 shims without executing a candidate or writing files. */
export function nativeDispatcherSources({ component, root = process.cwd() } = {}) {
  if (!/^[a-z][a-z0-9-]*$/.test(component ?? '')) throw Error('A component name is required.');
  const plan = JSON.parse(readFileSync(resolve(root, 'services', component, 'deploy.json'), 'utf8'));
  if (plan.componentId !== component) throw Error('The deploy plan does not match its component.');
  const entry = commandPath(plan.entrypoint), health = commandPath(plan.readiness?.command);
  if (entry === health) throw Error('Service and health dispatch paths must be distinct.');
  const shutdown = plan.shutdown?.timeoutMs;
  if (!Number.isSafeInteger(shutdown) || shutdown < 1) throw Error('The deploy plan needs its bounded shutdown timeout.');
  const files = [[entry, 'ivy-' + component, false], [health, 'ivy-health', true]].map(([path, name, health]) => {
    let module = relative(dirname(path), 'native/dispatch.mjs').replaceAll('\\', '/');
    if (!module.startsWith('.')) module = './' + module;
    // Dynamic import also works when the retained .js path is loaded as CommonJS.
    const unavailable = health ? 'process.stdout.write(\'{"ok":false,"code":"artifact_missing"}\\n\');' : 'process.stderr.write("Native dispatcher is unavailable.\\n");';
    const source = `import(${JSON.stringify(module)}).then(({dispatch,dispatchFailure})=>dispatch(${JSON.stringify(name)},${shutdown}).catch(error=>dispatchFailure(error,${health}))).catch(()=>{${unavailable}process.exitCode=1;});\n`;
    return { path, source };
  });
  return { component, files };
}

/** Emit the approved v1 legacy entrypoints. Native payload assembly and owner
 * launch/health qualification are separate from these small dispatchers. */
export function buildNativeDispatchers({ component, root = process.cwd(), outputRoot = resolve(root, 'dist') } = {}) {
  const rendered = nativeDispatcherSources({ component, root });
  outputRoot = resolve(outputRoot);
  mkdirSync(outputRoot, { recursive: true });
  if (realpathSync(outputRoot) !== outputRoot) throw Error('Native dispatcher output must not use directory links.');
  const launcher = resolve(outputRoot, 'native/dispatch.mjs');
  const files = rendered.files.map(({path, source}) => ({destination: resolve(outputRoot, path), source}));
  for (const destination of [launcher, ...files.map(file => file.destination)]) {
    mkdirSync(dirname(destination), { recursive: true });
    if (realpathSync(dirname(destination)) !== dirname(destination) || (existsSync(destination) && realpathSync(destination) !== destination)) throw Error('Native dispatcher output must not use links.');
  }
  copyFileSync(fileURLToPath(new URL('./native-dispatch.mjs', import.meta.url)), launcher);
  for (const { destination, source } of files) writeFileSync(destination, source);
  return { component, files: [launcher, ...files.map(file => file.destination)] };
}
