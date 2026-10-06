/** hasChildren false lets a tree omit the expand control without loading the empty branch. */
export interface HierarchyNode { id: string; label: string; href: string; icon?: string | null; hasChildren?: boolean }
export interface HierarchyPage { items: HierarchyNode[]; nextCursor: string | null }
export type HierarchyLoader = (parentId: string | null, cursor: string | null, signal: AbortSignal) => Promise<HierarchyPage>;
export type HierarchyReorder = (objectId: string, beforeObjectId: string | null) => Promise<void>;
/** Moves an Object under `parentId` (null: root), placing it before `beforeObjectId` or last. */
export type HierarchyMove = (objectId: string, parentId: string | null, beforeObjectId: string | null) => Promise<void>;
