import test from 'node:test';
import assert from 'node:assert/strict';
import { validateQualityCase, verifyQualityOutput } from '../support/native-quality.mjs';

const fixture = { schemaVersion: 1, choices: ['A', 'B'], answer: 'B', headers: ['Option', 'Dauer', 'Risiko'],
  rows: [['A', '1 Tag', 'niedrig'], ['B', '2 Tage', 'mittel'], ['C', '3 Tage', 'hoch']] };
const marker = 'synthetic-verifier-marker';
const table = [fixture.headers, ['---', '---', '---'], ...fixture.rows].map(row => '| ' + row.join(' | ') + ' |').join('\n');
const output = 'Auswahl: B\n' + table + '\n' + marker;

test('quality gate accepts the exact supplied comparison and rejects wrong choice, invented facts, omissions and duplicate rows', () => {
  assert.doesNotThrow(() => verifyQualityOutput(output, fixture, marker));
  for (const invalid of [output.replace('Auswahl: B', 'Auswahl: A'), output.replace('2 Tage', '20 Tage'),
    output.replace('| C | 3 Tage | hoch |', ''), output + '\n| C | 3 Tage | hoch |', output.replace(marker, ''),
    output.replace('| Option | Dauer | Risiko |', '| Option | Kosten | Risiko |')]) {
    assert.throws(() => verifyQualityOutput(invalid, fixture, marker), assert.AssertionError);
  }
});

test('quality fixture rejects ambiguous choices and cells that could inject additional table rows', () => {
  assert.throws(() => validateQualityCase({ ...fixture, answer: 'A' }), assert.AssertionError);
  assert.throws(() => validateQualityCase({ ...fixture, rows: [['A', '1 Tag\n| Zusatz |', 'niedrig'], ...fixture.rows.slice(1)] }), assert.AssertionError);
  assert.throws(() => validateQualityCase({ ...fixture, rows: [fixture.rows[0], fixture.rows[0], fixture.rows[2]] }), assert.AssertionError);
});
