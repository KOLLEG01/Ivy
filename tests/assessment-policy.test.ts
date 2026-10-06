import test from 'node:test';
import assert from 'node:assert/strict';
import { restrictAssessmentParameters, assertAssessmentPolicy } from '../packages/sdk/src/assessment-policy.js';

test('assessment overrides enabling config on start and resume and preserves model and input', () => {
  const original = { threadStart: { model: 'selected', config: { 'features.apps': true } }, threadResume: { config: { web_search: 'live' } }, turnStart: { input: [] } };
  const restricted = restrictAssessmentParameters(original);
  assert.equal(restricted.threadStart.model, 'selected'); assert.deepEqual(restricted.turnStart.input, []);
  assert.equal(restricted.threadStart.config['features.apps'], false); assert.equal(restricted.threadResume.config.web_search, 'disabled');
  assert.equal(original.threadStart.config['features.apps'], true); assertAssessmentPolicy(restricted);
  restricted.threadResume.config.web_search = 'live'; assert.throws(() => assertAssessmentPolicy(restricted), { code: 'assessment_policy_changed' });
});

test('assessment refuses mixing legacy permission fields into original parameters', () => {
  assert.throws(() => restrictAssessmentParameters({ threadStart: {}, threadResume: {}, turnStart: { sandbox: 'danger-full-access' } }), { code: 'assessment_policy_invalid' });
});
