// HE05's approved semantics: whole-stream ordered literal UTF-16 replacement.
// Chunk timing is deliberately excluded; the Rust sink emits bounded chunks.
let input = '';
for await (const chunk of process.stdin) input += chunk;
const cases = input.split('\n');
process.stdout.write(cases.map(value => {
  const [limit, secrets, chunks] = value.split('\t');
  const maximum = Number(limit);
  let emitted = new TextDecoder().decode(Buffer.concat(chunks.split(',').map(chunk => Buffer.from(chunk, 'hex'))));
  for (const units of secrets.split(';')) {
    const secret = units ? units.split(',').map(unit => String.fromCharCode(parseInt(unit, 16))).join('') : '';
    if (secret.length) emitted = emitted.replaceAll(secret, '[redacted]');
  }
  // UTF-8 sinks replace isolated surrogates after all UTF-16 replacement passes.
  const bytes = Buffer.from(emitted, 'utf8');
  return [Buffer.from(bytes.subarray(-maximum).toString('utf8')).toString('hex'),
    bytes.length > maximum, bytes.toString('hex')].join('\t');
}).join('\n'));
