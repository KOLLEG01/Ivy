import { createServer } from 'node:http';
import { createHash, randomBytes } from 'node:crypto';
import { readFile, mkdir } from 'node:fs/promises';
import { join } from 'node:path';
import Ajv2020 from 'ajv/dist/2020.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';

const upstream = 'https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas';
const sha = value => 'sha256:' + createHash('sha256').update(value).digest('hex');
const maximum = 1048576;
const contract = JSON.parse(await readFile(new URL('../../../specs/schemas/phone-media.schema.json', import.meta.url), 'utf8'));
const validators = new Ajv2020({ strict: false }), checkRequest = validators.compile({ ...contract, $ref: '#/$defs/CallRequest' }), checkReceipt = validators.compile({ ...contract, $ref: '#/$defs/CallReceipt' });
const required = (condition, code) => { if (!condition) throw Object.assign(Error(code), { code }); };
async function bytesOf(stream) {
  const chunks = []; let size = 0;
  for await (const chunk of stream) { const bytes = Buffer.from(chunk); size += bytes.length; required(size <= maximum, 'media_body_too_large'); chunks.push(bytes); }
  return Buffer.concat(chunks);
}

/** One authenticated call to one fixed TLS endpoint. This does not implement PhoneBridge yet. */
export async function nativeMediaAdapter(directory, fetcher = fetch) {
  await mkdir(directory, { recursive: true }); const path = join(directory, 'media-receipt.json'), token = randomBytes(32).toString('hex');
  let receipt = null, work = null;
  try {
    receipt = JSON.parse(await readFile(path, 'utf8')); required(checkReceipt(receipt), 'media_receipt_invalid');
    if (receipt.state === 'received') { const response = receipt.response; required(response, 'media_receipt_invalid'); const body = Buffer.from(response.bodyBase64, 'base64'); required(body.toString('base64') === response.bodyBase64 && body.length === response.bytes && sha(body) === response.bodyHash, 'media_receipt_invalid'); }
    else required(receipt.response === null, 'media_receipt_invalid');
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  const persist = async value => { required(checkReceipt(value), 'media_receipt_invalid'); await atomicJson(path, value); receipt = value; };
  const execute = async (bytes, authorization, headers) => {
    const requestHash = sha(bytes), authorizationHash = sha(authorization + '\0' + (headers['chatgpt-account-id'] ?? ''));
    if (receipt) {
      required(receipt.requestHash === requestHash && receipt.authorizationHash === authorizationHash, 'media_call_conflict');
      required(receipt.state === 'received', 'media_outcome_unknown'); return receipt.response;
    }
    const request = JSON.parse(bytes.toString('utf8')); required(checkRequest(request), 'media_request_invalid');
    // Only this concrete native/backend mismatch is adapted; all other session fields and SDP stay.
    delete request.session.model; const body = JSON.stringify(request);
    await persist({ schemaVersion: 1, requestHash, authorizationHash, forwardedHash: sha(body), startedAt: new Date().toISOString(), state: 'forwarding', response: null });
    try {
      const forwardedHeaders = { authorization, 'content-type': 'application/json' };
      for (const key of ['chatgpt-account-id', 'openai-alpha', 'x-oai-attestation', 'x-session-id', 'x-codex-turn-metadata', 'originator', 'user-agent', 'openai-beta', 'x-codex-thread-id', 'x-client-request-id']) if (typeof headers[key] === 'string') forwardedHeaders[key] = headers[key];
      const response = await fetcher(upstream, { method: 'POST', headers: forwardedHeaders, body, redirect: 'error', signal: AbortSignal.timeout(25000) });
      const received = response.body ? await bytesOf(response.body) : Buffer.alloc(0);
      const saved = { status: response.status, contentType: response.headers.get('content-type'), location: response.headers.get('location'), bodyHash: sha(received), bytes: received.length, bodyBase64: received.toString('base64') };
      await persist({ ...receipt, state: 'received', response: saved }); return saved;
    } catch (error) { await persist({ ...receipt, state: 'outcome_unknown', response: null }); throw Object.assign(Error('media_outcome_unknown'), { code: 'media_outcome_unknown' }); }
  };
  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url, 'http://127.0.0.1');
      required(request.method === 'POST' && url.pathname === '/' + token + '/backend-api/codex/realtime/calls' && url.searchParams.size === 2 && url.searchParams.get('intent') === 'quicksilver' && url.searchParams.get('architecture') === 'avas', 'media_not_found');
      required(!request.headers.origin && !request.headers.cookie && /^Bearer \S+$/.test(request.headers.authorization ?? '') && /^application\/json(?:;|$)/.test(request.headers['content-type'] ?? ''), 'media_not_admitted');
      const bytes = await bytesOf(request), authorization = request.headers.authorization;
      if (work) await work.catch(() => undefined);
      const selected = execute(bytes, authorization, request.headers); work = selected;
      let result; try { result = await selected; } finally { if (work === selected) work = null; }
      response.statusCode = result.status; if (result.contentType) response.setHeader('content-type', result.contentType); if (result.location) response.setHeader('location', result.location);
      response.end(Buffer.from(result.bodyBase64, 'base64'));
    } catch (error) {
      response.statusCode = error.code === 'media_not_found' ? 404 : error.code === 'media_not_admitted' ? 403 : error.code === 'media_body_too_large' ? 413 : ['media_call_conflict', 'media_outcome_unknown'].includes(error.code) ? 409 : 400;
      response.setHeader('content-type', 'application/json'); response.end(JSON.stringify({ error: { code: error.code ?? 'media_request_invalid', message: 'The original media request was not forwarded again.' } }));
    }
  });
  server.requestTimeout = 30000; server.headersTimeout = 5000;
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  return { baseUrl: 'http://127.0.0.1:' + server.address().port + '/' + token + '/backend-api/codex',
    summary() { if (!receipt) return null; const { authorizationHash, response, ...summary } = receipt; return { ...summary, removedFields: ['session.model'], response: response ? { status: response.status, contentType: response.contentType, bodyHash: response.bodyHash, bytes: response.bytes } : null }; },
    async close() { server.closeIdleConnections(); await new Promise(resolve => server.close(resolve)); await work?.catch(() => undefined); } };
}
