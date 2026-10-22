import type { TaskBoard } from '../../contracts/src/generated.js';
import type { RpcClient } from './client.js';
export type CommentEntry = {
    comment: TaskBoard.Comment;
    receipt: TaskBoard.CommentDelivery | null;
};
export type CommentIndex = {
    object: TaskBoard.ObjectPin;
    length: number;
};
export type QuestionIndex = {
    commentId: string;
    sequence: number;
    recipient?: TaskBoard.Comment['recipient'];
    open: boolean;
};
export type CommentSummary = {
    directCount: number;
    questions: QuestionIndex[];
};
export type CommentTree = {
    kind: 'tree';
    root: CommentIndex | null;
    length: number;
    summary?: CommentSummary;
};
export type CommentNode = {
    schemaVersion: 1;
    taskId: string;
} & ({
    kind: 'comment';
    entry: CommentEntry;
} | {
    kind: 'page';
    children: CommentIndex[];
});
export declare const commentNodeKey = "task-board/comment-node";
/** Immutable pins permit bounded caching, including historical ticket reads. */
export declare class CommentReader {
    readonly client: RpcClient;
    private readonly maximumBytes;
    private readonly cache;
    private bytes;
    constructor(client: RpcClient, maximumBytes?: number);
    remember(pin: TaskBoard.ObjectPin, node: CommentNode): void;
    node(pin: TaskBoard.ObjectPin, taskId: string, signal?: AbortSignal): Promise<CommentNode>;
    /** Range reads follow only intersecting index pages, never scan other Hive objects. */
    range(tree: CommentTree, taskId: string, start?: number, end?: number, signal?: AbortSignal): Promise<CommentEntry[]>;
}
