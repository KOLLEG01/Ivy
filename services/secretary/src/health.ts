import { configurationPath, instanceConfig, checkHealth } from '../../../packages/sdk/src/host.js';
import { IvyError } from '../../../packages/sdk/src/node.js';
try { const health = await checkHealth(await instanceConfig(configurationPath())); process.stdout.write(JSON.stringify({ ok: true, buildId: health.buildId, generation: health.generation }) + '\n'); }
catch (error) { process.stdout.write(JSON.stringify({ ok: false, code: IvyError.from(error).code }) + '\n'); process.exitCode = 1; }
