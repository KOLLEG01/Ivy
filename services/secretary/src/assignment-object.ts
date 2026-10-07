import { hashJson } from "../../../packages/sdk/src/node.js";
import type { Wire } from "../../../packages/sdk/src/node.js";
import type { Assignment, ObjectTrigger, Pin } from "./schema.js";

export const objectWindowKey = "secretary/object-window";
export interface ObjectWindow {
  assignment: Pin;
  assignmentSnapshot: Assignment;
  objectId: string;
  firstSequence: number;
  firstRevision: number;
  lastRevision: number;
  occurredAt: string;
  dueAt: string;
  before: Wire.Json;
  after: Wire.Json;
}
export interface ObjectWindowState {
  assignmentId: string;
  objectId: string;
  lastSequence: number;
  fingerprint: string;
  projection: Wire.Json;
  revision: number;
  pending: ObjectWindow | null;
}
export function pointer(value: Wire.Json, path: string): Wire.Json {
  let current = value;
  if (path === "") return current;
  for (const token of path
    .slice(1)
    .split("/")
    .map((part) => part.replace(/~1/g, "/").replace(/~0/g, "~"))) {
    if (
      current === null ||
      typeof current !== "object" ||
      !Object.hasOwn(current, token)
    )
      return null;
    current = (current as Record<string, Wire.Json>)[token]!;
  }
  return current;
}
export const objectProjection = (
  value: Wire.Json,
  trigger: ObjectTrigger,
): Wire.Json =>
  trigger.paths.length
    ? Object.fromEntries(
        trigger.paths.map((path) => [path, pointer(value, path)]),
      )
    : value;
export function observationReady(
  value: Wire.Json,
  trigger: ObjectTrigger,
  dueAt: string,
  now: Date,
): boolean {
  if (!trigger.observation) return true;
  const condition = trigger.observation,
    at = pointer(value, condition.observedAtPath);
  const timestamp = typeof at === "string" ? Date.parse(at) : NaN;
  return (
    pointer(value, condition.completePath) === true &&
    Number.isFinite(timestamp) &&
    timestamp >= Date.parse(dueAt) &&
    timestamp <= now.getTime() + 5000 &&
    now.getTime() - timestamp <= condition.maximumAgeSeconds * 1000
  );
}
export const projectionChanged = (left: Wire.Json, right: Wire.Json): boolean =>
  hashJson(left) !== hashJson(right);
