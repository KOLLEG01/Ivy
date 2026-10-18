import { daemonObservation } from '../../dist/packages/host-runtime/src/codex-daemon.js';
import { IvyError } from '../../dist/packages/contracts/src/errors.js';
let input = '';
for await (const chunk of process.stdin) input += chunk;
console.log(JSON.stringify(JSON.parse(input).map(value => {
  try { return { result: daemonObservation(value, '0.154.0') }; }
  catch (error) {
    const failure = IvyError.from(error);
    return { code: failure.code, message: failure.message, outcome: failure.outcome };
  }
})));
