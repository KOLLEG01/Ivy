import type { Json } from './canonical.js';
export type Content = {
    encoding: 'json';
    value: Json;
} | {
    encoding: 'text' | 'base64';
    value: string;
};
export type Schema = boolean | {
    [keyword: string]: unknown;
};
export type ObjectRetention = {
    mode: 'retain';
} | {
    mode: 'expire';
    maximumAgeDays: number;
} | {
    mode: 'owned';
};
export type RevisionRetention = {
    mode: 'all';
} | {
    mode: 'current';
} | ({
    mode: 'bounded';
} & ({
    maximumCount: number;
    maximumAgeDays?: number;
} | {
    maximumCount?: number;
    maximumAgeDays: number;
}));
export interface RetentionPolicy {
    objects: ObjectRetention;
    revisions: RevisionRetention;
}
export interface RevisionReference {
    objectId: string;
    revision: number;
}
export type RevisionReferences = Record<string, RevisionReference>;
export interface DataContract {
    key: string;
    version: string;
    owner: {
        kind: 'hive';
    } | {
        kind: 'agent';
    } | {
        kind: 'service';
        serviceName: string;
    };
    mediaType: string;
    retention: RetentionPolicy;
    specMarkdown: string;
    jsonSchema?: Schema;
}
export interface ObjectMetadata {
    id: string;
    parentId: string | null;
    ownerObjectId: string | null;
    name: string;
    path: string;
    contractKey: string;
    position: number;
    icon: string | null;
    currentRevision: number;
    contractVersion: string;
    archivedAt: string | null;
    effectivelyArchived: boolean;
    createdAt: string;
    updatedAt: string;
}
export interface RevisionMetadata {
    objectId: string;
    revision: number;
    contractVersion: string;
    contentHash: string;
    mediaType: string;
    byteLength: number;
    createdAt: string;
    references: RevisionReferences;
}
export interface ObjectRead {
    object: ObjectMetadata;
    revision: RevisionMetadata;
    content: Content;
}
export interface ObjectWriteResult {
    object: ObjectMetadata;
    revision: RevisionMetadata;
}
export type ObjectWrite = {
    mutationId: string;
    contractVersion: string;
    content: Content;
    references: RevisionReferences;
} & ({
    create: {
        contractKey: string;
        parentId: string | null;
        ownerObjectId: string | null;
        name: string;
        icon?: string | null;
    };
    objectId?: never;
    expectedRevision?: never;
} | {
    objectId: string;
    expectedRevision: number;
    create?: never;
});
export interface ResourceRef {
    serviceNodeId: string;
    namespace: string;
    kind: string;
    nativeId: string;
}
export type Scalar = null | boolean | number | string;
export type Predicate = {
    op: 'and' | 'or';
    args: Predicate[];
} | {
    op: 'not';
    arg: Predicate;
} | {
    op: 'isNull';
    field: string;
} | {
    op: 'in';
    field: string;
    value: Scalar[];
} | {
    op: 'contains';
    field: string;
    value: string;
} | {
    op: 'eq' | 'ne' | 'gt' | 'gte' | 'lt' | 'lte';
    field: string;
    value: Scalar;
};
export interface ObjectQuery {
    contractKey: string;
    contractVersions?: string[];
    where?: Predicate;
    select?: string[];
    orderBy?: {
        field: string;
        direction: 'asc' | 'desc';
    }[];
    includeArchived?: boolean;
    limit?: number;
    cursor?: string;
}
export interface QueryItem {
    objectId: string;
    revision: number;
    contractVersion: string;
    values: Record<string, Scalar>;
}
export interface Page<T> {
    items: T[];
    nextCursor: string | null;
    historyComplete?: boolean;
}
export interface EventFilter {
    topics?: string[];
    sources?: string[];
    objectIds?: string[];
}
export interface HiveEvent {
    sequence: number;
    topic: string;
    topicVersion: string;
    source: string;
    occurredAt: string;
    mutationId: string;
    payload: Json;
}
export interface EventGap {
    prunedThroughSequence: number;
    resumeAfterSequence: number;
}
export interface EventBatch {
    items: HiveEvent[];
    throughSequence: number;
    hasMore: boolean;
    gap: EventGap | null;
}
