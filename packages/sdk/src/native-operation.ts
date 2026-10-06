import { requireThat } from '../../contracts/src/errors.js';
import type { Agent } from '../../contracts/src/generated.js';
import { nativeOperationTerminal, validateNativeProgress } from './native-evidence.js';
import type { NativeOwner, NativeOperationCall } from './native-owner.js';

export interface NativeOperationJournal<T> {
  current: T;
  previous: Agent.Operation | null;
  /** A compact observed marker is sufficient to forbid replay even without a complete receipt. */
  wasObserved: boolean;
  retain(observation: Agent.Operation): Promise<T>;
}
export type NativeReconciliation<T> = { kind: 'observed'; value: T; observation: Agent.Operation }
  | { kind: 'absent'; value: T; absence: Agent.OperationAbsence };

/** One original operation, no scheduler and no implicit dispatch authority or retry identity. */
export async function reconcileNativeOperation<T>(options: {
  owner: Pick<NativeOwner, 'operation' | 'checkOperation'>;
  call: NativeOperationCall;
  journal: NativeOperationJournal<T>;
  /** Invoked only for owner-confirmed absence of a never-observed operation. Guards remain mandatory here. */
  onAbsent?: (call: NativeOperationCall, absence: Agent.OperationAbsence) => Promise<Agent.Operation | void>;
  absenceConflictCode?: string;
}): Promise<NativeReconciliation<T>> {
  const call = structuredClone(options.call), { journal, owner } = options, previous = journal.previous ? structuredClone(journal.previous) : null;
  if (previous) {
    owner.checkOperation(previous, call);
    if (nativeOperationTerminal(previous)) return { kind: 'observed', value: journal.current, observation: previous };
  }
  let observed = await owner.operation(call);
  if ('kind' in observed) {
    requireThat(!previous && !journal.wasObserved, options.absenceConflictCode ?? 'native_absence_conflict',
      'Owner absence cannot erase a previously observed operation or authorize replay.');
    if (!options.onAbsent) return { kind: 'absent', value: journal.current, absence: observed };
    const submitted = await options.onAbsent(structuredClone(call), observed);
    observed = submitted ?? await owner.operation(call);
    // This is still the original identity. A later pass must reconcile it; never invent a receipt.
    if ('kind' in observed) return { kind: 'absent', value: journal.current, absence: observed };
  }
  owner.checkOperation(observed, call);
  if (previous) validateNativeProgress(previous, observed);
  const value = previous && previous.updatedAt === observed.updatedAt && previous.phase === observed.phase ? journal.current : await journal.retain(structuredClone(observed));
  return { kind: 'observed', value, observation: observed };
}
