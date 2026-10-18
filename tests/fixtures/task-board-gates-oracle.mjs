import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
import {DatabaseSync} from 'node:sqlite';

const source = readFileSync('services/task-board/src/runtime/store.ts', 'utf8');
const schema = source.match(/\.exec\(`(PRAGMA journal_mode=WAL;[\s\S]*?)`\);/)[1];
const methods = source.slice(source.indexOf('  pendingCoordination()'), source.indexOf('  private async retainedLocalRoot(')) +
  source.slice(source.indexOf('  retiredOperation('), source.indexOf('  async collectLocal('));
const Store = new Function(stripTypeScriptTypes('class Store { local; constructor(db) {this.local=db;}\n' + methods + '\n}') + '\nreturn Store;')();
const db = new DatabaseSync(process.argv[3]);
db.exec(schema);
const store = new Store(db);
if (process.argv[2] === 'create') {
  const insert = db.prepare('INSERT INTO active_gates VALUES(?,?)');
  for (const [gate, operation] of [['z','op-z'],['a','op-a'],['é','op-e'],['💬','op-wide']]) insert.run(gate, operation);
  db.prepare('INSERT INTO retired_operations VALUES(?,?,?,?)').run('caller\ud800','retired','sha256:original','retired-name');
  store.retainOperationGates('op\udfff', ['z','a','z','é','💬','name\ud800']);
} else if (process.argv[2] !== 'inspect') throw Error('Unknown fixture action');
const pending = store.pendingCoordination();
const pages = [store.pendingCoordinationPage(null, 2),store.pendingCoordinationPage('a', 10),store.pendingCoordinationPage('z', 0)];
const gates = store.operationGates('op\udfff');
const retired = [store.retiredOperation('caller\ud800','retired'),store.retiredOperation('other','retired')];
const rows = db.prepare('SELECT operation_id,gate_name FROM operation_gates ORDER BY operation_id,gate_name').all();
process.stdout.write(JSON.stringify({pending,pages,gates,retired,rows}));
db.close();
