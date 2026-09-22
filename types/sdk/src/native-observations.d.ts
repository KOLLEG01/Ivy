import type { NativeContract } from '../../contracts/src/native-contract.js';
import type { Wire } from '../../contracts/src/generated.js';
/** Capability follows the retained schema, including an optional flag whose absence denies input. */
export declare function nativeThreadState(contract: NativeContract, result: unknown): {
    thread: Record<string, Wire.Json>;
    canStartTurn: boolean;
    state: string;
};
/** Interpret a complete page; wrapped entries must each name the exact requested turn. */
export declare function nativeTurnItems(contract: NativeContract, result: unknown, turnId: string): Record<string, Wire.Json>[];
