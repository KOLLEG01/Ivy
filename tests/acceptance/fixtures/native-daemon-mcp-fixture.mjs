import { createInterface } from 'node:readline';

// A bounded, read-only MCP fixture. It exposes only whether the shared daemon
// propagated this fixture's configured identity or an instance-only credential.
for await (const line of createInterface({ input: process.stdin, crlfDelay: Infinity })) {
  const request = JSON.parse(line); if (!Object.hasOwn(request, 'id')) continue;
  let result;
  if (request.method === 'initialize') result = { protocolVersion: request.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'ivy-daemon-identity-fixture', version: '1.0.0' } };
  else if (request.method === 'ping') result = {};
  else if (request.method === 'tools/list') result = { tools: [{ name: 'identity', description: 'Read isolated daemon identity flags.', inputSchema: { type: 'object', properties: {}, additionalProperties: false } }] };
  else if (request.method === 'tools/call' && request.params.name === 'identity') result = { content: [{ type: 'text', text: JSON.stringify({ sharedIdentity: process.env.IVY_TEST_IDENTITY === 'shared-home', instanceCredentialPresent: Boolean(process.env.IVY_HIVE_TOKEN) }) }] };
  else { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, error: { code: -32601, message: 'Unknown fixture method' } }) + '\n'); continue; }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
}
