import type { Agent, Wire } from '../../contracts/src/generated.js';
import type { NativeOwner } from './native-owner.js';
type Record = {
    [key: string]: Wire.Json;
};
export interface NativeTurnItemPages {
    format: 'native-turn-item-pages-1';
    snapshot: Agent.ReadObservation;
    pages: Agent.ReadObservation[];
}
/** Complete original evidence: native item pages, or the supported full-turn fallback. */
export type NativeTurnContents = Agent.ReadObservation | NativeTurnItemPages;
export declare const nativeTurnSnapshot: (value: NativeTurnContents) => Agent.ReadObservation;
/** Verify original observations without synthesizing a native full-history reply. */
export declare function verifyNativeTurnContents(owner: NativeOwner, value: NativeTurnContents, threadId: string, turnId: string): Promise<{
    snapshot: Agent.ReadObservation;
    turn: Record;
    items: Record[];
}>;
/** Read contents only when requested; an exact first-page -32601 retains full-turn support. */
export declare function readNativeTurnContents(owner: NativeOwner, snapshot: Agent.ReadObservation, threadId: string, turnId: string, maximumBytes?: number): Promise<NativeTurnContents>;
export {};
