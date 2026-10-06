import type { Secretary } from "../../../packages/sdk/src/client.js";

export type Pin = Secretary.Pin;
export type ContactWindow = Secretary.ContactWindow;
export type ContactRule = Secretary.ContactRule;
export type VoiceRule = Secretary.VoiceRule;
export type GlobalRules = Secretary.GlobalRules;
export type RuleOverrides = Secretary.RuleOverrides;
export type ExecutionTarget = Secretary.ExecutionTarget;
export type SecretaryConfiguration = Secretary.SecretaryConfiguration;
export type ScheduleTrigger = Secretary.ScheduleTrigger;
export type EventTrigger = Secretary.EventTrigger;
export type AssignmentTrigger = Secretary.AssignmentTrigger;
export type Assignment = Secretary.Assignment;
export type Execution = Secretary.Execution;
export interface Workspace {
  scope: Secretary.Scope;
  configuration: Pin | null;
  observedAt: string;
}
export interface Document<T> {
  pin: Pin;
  value: T;
  name: string;
}
