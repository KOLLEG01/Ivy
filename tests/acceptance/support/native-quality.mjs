import assert from 'node:assert/strict';

export function validateQualityCase(value) {
  assert.equal(value.schemaVersion, 1);
  assert.deepEqual(value.choices, ['A', 'B']);
  assert.equal(value.answer, 'B');
  assert.equal(value.headers.length, 3);
  assert.equal(value.rows.length, 3);
  for (const row of [value.headers, ...value.rows]) {
    assert.equal(row.length, 3);
    for (const cell of row) assert.ok(typeof cell === 'string' && cell.length > 0 && cell.length <= 160 && !/[|\r\n]/.test(cell));
  }
  assert.equal(new Set(value.rows.map(row => row[0])).size, 3);
  return value;
}

export function qualityPrompt(value, marker) {
  validateQualityCase(value);
  return 'Dies ist eine isolierte synthetische Abnahme. Stelle mit request_user_input genau eine Frage mit den Optionen A und B und warte auf die Antwort. '
    + 'Erst danach: Bestätige die tatsächlich gewählte Option mit "Auswahl: <Option>" und gib die folgenden gelieferten Fakten als deutsche Markdown-Tabelle mit exakt drei Datenzeilen unverändert wieder. '
    + 'Erfinde keine weiteren Fakten oder Empfehlungen. Gib abschließend den Marker ' + marker + ' aus. '
    + 'Verwende keine anderen Werkzeuge, lies oder ändere keine Dateien und führe keine externen Aktionen aus. '
    + 'Tabellenkopf und Daten: ' + JSON.stringify({ headers: value.headers, rows: value.rows });
}

export function verifyQualityOutput(text, value, marker) {
  validateQualityCase(value);
  assert.ok(text.includes(marker), 'Original quality marker is missing');
  assert.match(text, /Auswahl:\s*B(?:\s|$)/, 'The actual supplied answer B must be acknowledged');
  const rows = text.split(/\r?\n/).filter(line => line.trim().startsWith('|')).map(line => line.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map(cell => cell.trim()));
  assert.equal(rows.length, 5, 'Exactly one header, separator and three data rows are required');
  assert.deepEqual(rows[0], value.headers);
  assert.equal(rows[1].length, 3);
  assert.ok(rows[1].every(cell => /^:?-{3,}:?$/.test(cell)), 'Invalid table separator');
  assert.deepEqual(rows.slice(2), value.rows, 'Saved comparison must preserve every supplied fact without extra rows');
}
