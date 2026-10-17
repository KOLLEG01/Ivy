// Execute the unchanged TypeScript journal with only its import wiring adapted.
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';

const moduleUrl = source => 'data:text/javascript;base64,' + Buffer.from(stripTypeScriptTypes(source, { mode: 'transform' })).toString('base64');
const errors = moduleUrl(readFileSync('packages/contracts/src/errors.ts', 'utf8'));
const source = readFileSync('services/chat-bridge/src/whatsapp/journal.ts', 'utf8')
  .replace("'../../../../packages/sdk/src/node.js'", JSON.stringify(errors));
const { WhatsAppJournal } = await import(moduleUrl(source));
const actions = JSON.parse(readFileSync(0, 'utf8'));
const journal = new WhatsAppJournal(':memory:');
Date.now = () => 1000;
const snapshot = () => Object.fromEntries(['kv', 'inbox', 'outgoing', 'retired_inbox', 'retired_outgoing'].map(table => [table,
  journal.db.prepare(`SELECT * FROM ${table} ORDER BY ${table === 'kv' ? 'key' : 'id'}`).all().map(row => Object.values(row))]));
const apply = a => {
  switch (a.op) {
    case 'accept': journal.accept({ id:a.id, sender:'sender', jid:'fixture@invalid', message:a.message, received:a.at }); break;
    case 'set': journal.set(a.key, a.value); break;
    case 'save': journal.saveRequest(a.id, a.value); break;
    case 'rebind': journal.rebindRequest(a.id, a.previous, a.next); break;
    case 'finish': journal.finish(a.id); break;
    case 'enqueue': return journal.enqueue(a.key, 'fixture@invalid', a.text, a.automatic ?? false);
    case 'mark': for (const row of journal.outgoing()) if (!a.text || row.text === a.text) journal.mark(row.id, a.state); break;
    case 'protect': journal.set(a.key, a.cursor ? {ids:journal.outgoing().map(r => r.id),at:0} : journal.outgoing().map(r => r.id)); break;
    case 'prune': journal.historyLimits = {count:a.count,bytes:a.bytes}; journal.prune(); break;
    case 'transaction': journal.transaction(() => { for (const action of a.actions) apply(action); }); break;
    case 'snapshot': return snapshot();
    default: throw Error('Unknown test action');
  }
  return null;
};
const results = actions.map(a => { try { return {value:apply(a)}; } catch (error) { return {error:error.code}; } });
await journal.close();
process.stdout.write(JSON.stringify(results));
