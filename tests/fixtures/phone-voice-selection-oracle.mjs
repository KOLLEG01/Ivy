import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
import {IvyError, requireThat} from '../../packages/contracts/src/errors.ts';
const source = readFileSync(new URL('../../services/phone-bridge/src/runtime/voice-selection.ts', import.meta.url), 'utf8')
  .replace(/^import[^\n]*\n/, '').replace(/\bexport /g, '');
const api = new Function('IvyError', 'requireThat', stripTypeScriptTypes(source) +
  ';return {selectedPhoneVoiceModel,validatePhoneVoiceSelection,defaultPhoneVoiceSelection};')(IvyError, requireThat);
const rows = JSON.parse(readFileSync(0, 'utf8'));
const results = rows.map(row => {
  try {
    if (row.action === 'default') return api.defaultPhoneVoiceSelection;
    if (row.action === 'keypad') return api.selectedPhoneVoiceModel(row.model, row.effort);
    api.validatePhoneVoiceSelection(row.selection, row.models);
    return null;
  } catch (error) { return {code:error.code, message:error.message}; }
});
process.stdout.write(JSON.stringify(results));
