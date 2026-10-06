import { canonical } from '../../contracts/src/canonical.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Wire } from '../../contracts/src/generated.js';

/** Best-effort restrictions of the unchanged host, NOT a claim of tool isolation. */
export const assessmentToolConfiguration: Record<string, Wire.Json> = Object.freeze({
  'features.shell_tool': false, 'features.apps': false, 'features.multi_agent': false, web_search: 'disabled',
});
type Parameters = { threadStart: Record<string, Wire.Json>; threadResume: Record<string, Wire.Json>; turnStart: Record<string, Wire.Json> };
export function restrictAssessmentParameters<T extends Parameters>(original: T): T {
  const value = structuredClone(original);
  for (const params of [value.threadStart, value.threadResume, value.turnStart]) {
    requireThat(!('sandbox' in params) && !('sandboxPolicy' in params), 'assessment_policy_invalid', 'Assessment uses the named permissions profile without legacy sandbox fields.');
    Object.assign(params, { permissions: ':danger-full-access', approvalPolicy: 'never' });
  }
  for (const params of [value.threadStart, value.threadResume]) {
    const config = params['config'];
    requireThat(config === undefined || config !== null && typeof config === 'object' && !Array.isArray(config), 'assessment_policy_invalid', 'Assessment config must be an object.');
    params['config'] = { ...(config as Record<string, Wire.Json> | undefined), ...assessmentToolConfiguration };
  }
  Object.assign(value.threadStart, { environments: [], selectedCapabilityRoots: [], dynamicTools: [] });
  value.turnStart['environments'] = [];
  return value;
}
export function assertAssessmentPolicy(params: Parameters): void {
  requireThat(canonical(params) === canonical(restrictAssessmentParameters(params)), 'assessment_policy_changed', 'Original assessment parameters lack the configured restrictions; do not dispatch them.');
}
