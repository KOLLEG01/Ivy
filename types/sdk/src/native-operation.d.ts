import type { Agent } from '../../contracts/src/generated.js';
import type { NativeOwner, NativeOperationCall } from './native-owner.js';
export interface NativeOperationJournal<T> {
    current: T;
    previous: Agent.Operation | null;
    /** A compact observed marker is sufficient to forbid replay even without a complete receipt. */
    wasObserved: boolean;
    retain(observation: Agent.Operation): Promise<T>;
}
export type NativeReconciliation<T> = {
    kind: 'observed';
    value: T;
    observation: Agent.Operation;
} | {
    kind: 'absent';
    value: T;
    absence: Agent.OperationAbsence;
};
/** One original operation, no scheduler and no implicit dispatch authority or retry identity. */
export declare function reconcileNativeOperation<T>(options: {
    owner: Pick<NativeOwner, 'operation' | 'checkOperation'>;
    call: NativeOperationCall;
    journal: NativeOperationJournal<T>;
    /** Invoked only for owner-confirmed absence of a never-observed operation. Guards remain mandatory here. */
    onAbsent?: (call: NativeOperationCall, absence: Agent.OperationAbsence) => Promise<Agent.Operation | void>;
    absenceConflictCode?: string;
}): Promise<NativeReconciliation<T>>;
