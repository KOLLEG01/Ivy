import { inspect } from 'node:util';

// Protocol output and diagnostics are held only until a failure, never written to a log.
export default async function* report(events) {
  const output = new Map(), failed = new Map();
  let actualTests = 0, filesReported = false;
  for await (const { type, data } of events) {
    const file = data.file ?? '';
    if (['test:stdout', 'test:stderr', 'test:diagnostic'].includes(type)) {
      if (failed.has(file)) {
        const text = String(data.message ?? '').slice(0, failed.get(file));
        if (text) yield text + '\n';
        failed.set(file, Math.max(0, failed.get(file) - text.length));
        continue;
      }
      output.set(file, ((output.get(file) ?? '') + (data.message ?? '') + '\n').slice(-6000));
    } else if (type === 'test:fail') {
      if (data.details?.error?.failureType === 'subtestsFailed') continue;
      yield `FAIL ${file}${data.line ? ':' + data.line : ''} ${data.name}\n`;
      const error = data.details?.error;
      yield inspect(error?.cause ?? error ?? data.details, { depth: 4, maxArrayLength: 15, maxStringLength: 1200 }).slice(0, 5000) + '\n';
      const text = (output.get(file) ?? '') + (file ? output.get('') ?? '' : '');
      if (text.trim()) yield text.slice(-6000) + '\n';
      output.delete(file); failed.set(file, 6000);
    } else if (type === 'test:pass' && process.env.IVY_TEST_PROGRESS === '1') {
      yield `${data.skip ? 'SKIP' : 'PASS'} ${data.name}\n`;
    } else if (type === 'test:summary') {
      if (data.file) { output.delete(data.file); failed.delete(data.file); actualTests += data.counts.tests; filesReported = true; }
      if (!data.file && (filesReported ? actualTests : data.counts?.tests) === 0) {
        process.exitCode = 1;
        yield 'FAIL no tests executed (check the selection/name filter).\n';
      }
    }
  }
}
