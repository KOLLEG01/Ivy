import type { TaskBoard } from './generated.js';

// Before task 1.5.0 and configuration 1.1.0 a requirement could name one capability instead of a host.
type StoredRequirement = TaskBoard.ExecutionRequirement | { kind: 'capability'; capabilityKey: string } | null;

/** A stored capability requirement now means automatic placement on a host that provides it. */
export function upgradeRequirement(requirement: StoredRequirement): TaskBoard.ExecutionRequirement | null {
  return requirement?.kind === 'capability' ? { kind: 'automatic' } : requirement;
}

/** Reads a Task revision in the current field model; revisions without a capability requirement are unchanged. */
export function upgradeTaskRecord(task: TaskBoard.Task): TaskBoard.Task {
  const requirement = task.fields.executionRequirement as StoredRequirement;
  if (requirement?.kind !== 'capability') return task;
  return { ...task, fields: { ...task.fields, executionRequirement: { kind: 'automatic' },
    requiredCapabilities: [...new Set([...(task.fields.requiredCapabilities ?? []), requirement.capabilityKey])] } };
}

export function upgradeConfigurationRecord(configuration: TaskBoard.Configuration): TaskBoard.Configuration {
  const requirement = configuration.defaults.executionRequirement as StoredRequirement;
  return requirement?.kind === 'capability'
    ? { ...configuration, defaults: { ...configuration.defaults, executionRequirement: upgradeRequirement(requirement) } }
    : configuration;
}
