import type { TaskBoard } from './generated.js';
type StoredRequirement = TaskBoard.ExecutionRequirement | {
    kind: 'capability';
    capabilityKey: string;
} | null;
/** A stored capability requirement now means automatic placement on a host that provides it. */
export declare function upgradeRequirement(requirement: StoredRequirement): TaskBoard.ExecutionRequirement | null;
/** Reads a Task revision in the current field model; revisions without a capability requirement are unchanged. */
export declare function upgradeTaskRecord(task: TaskBoard.Task): TaskBoard.Task;
export declare function upgradeConfigurationRecord(configuration: TaskBoard.Configuration): TaskBoard.Configuration;
export {};
