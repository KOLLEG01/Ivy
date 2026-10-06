import { isAbsolute } from 'node:path';
import { hashJson } from '../../../packages/sdk/src/node.js';
import { need, validate } from './schema.js';
import type { Assignment, AssignmentTrigger, ContactWindow, ExecutionTarget, GlobalRules, RuleOverrides, SecretaryConfiguration, Settings } from './schema.js';

const timeZone = (value: string): void => {
  try { new Intl.DateTimeFormat('en', { timeZone: value }).format(); }
  catch { need(false, 'secretary_assignment_invalid', 'Secretary rules and schedules need a supported IANA time zone.'); }
};

const window = (value: ContactWindow | null): void => {
  if (!value) return;
  timeZone(value.timeZone);
  need(value.start !== value.end, 'secretary_assignment_invalid', 'A Secretary contact window needs distinct start and end times.');
};

export function validateRules(value: GlobalRules): void {
  validate('GlobalRules', value);
  window(value.main.window); window(value.whatsapp.window); window(value.voice.window);
}

/**
 * `whatsapp` remains in the registered 1.0 wire contract, but it is not a
 * separate contact target. Main always means the selected target's primary
 * conversation, including a WhatsApp primary conversation.
 */
export function unifiedMainRules(value: GlobalRules): GlobalRules {
  const main = structuredClone(value.main);
  return { ...structuredClone(value), main, whatsapp: { ...main, questionsOnly: false } };
}

export function unifiedRuleOverrides(value: RuleOverrides): RuleOverrides {
  const result = structuredClone(value);
  delete result.whatsapp;
  return result;
}

export function validateExecutionTarget(value: ExecutionTarget): void {
  validate('ExecutionTarget', value);
  need(isAbsolute(value.threadCwd), 'secretary_assignment_invalid', 'Secretary task working directories must be absolute.');
}

export function validateConfiguration(value: SecretaryConfiguration): void {
  validate('SecretaryConfiguration', value); validateRules(value.rules);
  if (value.execution) validateExecutionTarget(value.execution);
}

export function validateTrigger(value: AssignmentTrigger): void {
  validate('AssignmentTrigger', value);
  if (value.kind === 'event') return;
  timeZone(value.timeZone);
  need(value.cadence === 'interval'
    ? value.intervalMinutes !== null && value.localTime === null && value.weekdays.length === 0
    : value.intervalMinutes === null && value.localTime !== null && (value.cadence === 'daily' ? value.weekdays.length === 0 : value.weekdays.length > 0),
  'secretary_assignment_invalid', 'The Secretary schedule fields must match its selected cadence.');
  need(new Set(value.weekdays).size === value.weekdays.length, 'secretary_assignment_invalid', 'Secretary schedule weekdays must be unique.');
}

export function validateAssignment(value: Assignment): void {
  validate('Assignment', value); validateTrigger(value.trigger);
  window(value.rules.main?.window ?? null); window(value.rules.voice?.window ?? null);
  need(value.assignmentId.length <= 256, 'secretary_assignment_invalid', 'Secretary assignment identities are too long.');
}

const mergePart = <T extends object>(base: T, override: Partial<T> | undefined): T => ({ ...base, ...(override ?? {}) });
export function effectiveRules(defaults: GlobalRules, overrides: RuleOverrides): GlobalRules {
  const base = unifiedMainRules(defaults), selected = unifiedRuleOverrides(overrides);
  const value: GlobalRules = {
    main: mergePart(base.main, selected.main),
    whatsapp: { ...mergePart(base.main, selected.main), questionsOnly: false },
    voice: mergePart(base.voice, selected.voice),
    researchMaxMinutes: selected.researchMaxMinutes ?? base.researchMaxMinutes,
  };
  validateRules(value); return value;
}

const allowedWindow = (settings: Settings): ContactWindow | null => settings.policy.silentTime ? {
  timeZone: settings.policy.silentTime.timeZone,
  start: settings.policy.silentTime.end,
  end: settings.policy.silentTime.start,
} : null;

export function defaultConfiguration(settings: Settings, now: string, execution: ExecutionTarget | null = null): SecretaryConfiguration {
  const contactWindow = allowedWindow(settings);
  const main = { enabled: true, minimumUrgency: settings.policy.minimumMainUrgency, window: contactWindow } as const;
  const value: SecretaryConfiguration = { schemaVersion: 1, execution, updatedAt: now, rules: {
    main,
    whatsapp: { ...main, questionsOnly: false },
    voice: { enabled: true, minimumUrgency: 'critical', window: contactWindow, immediateOnly: true },
    researchMaxMinutes: settings.policy.researchMaxMinutes,
  } };
  validateConfiguration(value); return value;
}

export const assignmentName = (assignmentId: string) => 'assignment-' + hashJson(assignmentId).slice('sha256:'.length);
export const executionName = (assignmentId: string, triggerKey: string) => 'execution-' + hashJson([assignmentId, triggerKey]).slice('sha256:'.length);
