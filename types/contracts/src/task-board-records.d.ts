import type { TaskBoard } from './generated.js';
type StoredRequirement = TaskBoard.ExecutionRequirement | {
    kind: 'capability';
    capabilityKey: string;
} | null;
/** A stored capability requirement now means automatic placement on a host that provides it. */
export declare function upgradeRequirement(requirement: StoredRequirement): TaskBoard.ExecutionRequirement | null;
/** Historical structured planning becomes ordinary context; it never gates execution. */
export declare function upgradeCommentRecord(comment: TaskBoard.Comment): TaskBoard.Comment;
export declare function upgradeTaskRecord(value: TaskBoard.Task): TaskBoard.Task;
export declare function upgradeRunRecord(run: TaskBoard.Run): TaskBoard.Run;
export declare function upgradeConfigurationRecord(configuration: TaskBoard.Configuration): TaskBoard.Configuration;
export declare function upgradeResultContent(content: TaskBoard.ResultContent): TaskBoard.ResultContent;
export declare function upgradeResultRecord(result: TaskBoard.Result): TaskBoard.Result;
export {};
