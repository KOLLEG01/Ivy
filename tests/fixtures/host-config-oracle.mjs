import {existsSync, readFileSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js')) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const {jsonFile, instanceConfig, configurationPath} = await import('../../packages/host-runtime/src/config.ts');
const {IvyError} = await import('../../packages/contracts/src/errors.ts');
const input = JSON.parse(readFileSync(0, 'utf8'));
try {
  let value;
  if (input.kind === 'file') value = await jsonFile(input.path, input.limit);
  else if (input.kind === 'health') {
    const {checkHealth} = await import('../../packages/host-runtime/src/health.ts');
    Date.now = () => input.now;
    value = await checkHealth(input.config, input.endpoint, input.verifyRegistration);
  }
  else if (input.kind === 'endpoint') {
    const {localHiveEndpoint} = await import('../../packages/host-runtime/src/health.ts');
    value = localHiveEndpoint(input.config, input.docker).href;
  }
  else if (input.kind === 'instance') value = await instanceConfig(input.path);
  else if (input.kind === 'path') {
    process.argv = [process.execPath, 'fixture', ...input.args];
    if (input.environment === null) delete process.env.IVY_INSTANCE_CONFIG;
    else process.env.IVY_INSTANCE_CONFIG = input.environment;
    value = configurationPath();
  } else throw Error('Unknown fixture operation');
  process.stdout.write(JSON.stringify({ok:true,value}));
} catch (error) {
  process.stdout.write(JSON.stringify({ok:false,error:IvyError.from(error).toWire()}));
}
