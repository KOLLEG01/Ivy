import type { ValidateFunction } from 'ajv';
import type { Schema } from './types.js';
export declare function pointerParts(pointer: string): string[];
/** Admit a finite schema graph. Recursive refs must consume an object property or array item. */
export declare function admitSchema(schema: Schema): void;
export declare class SchemaValidators {
    private readonly cache;
    private readonly immutable;
    private anonymousCompiler;
    private compilations;
    compile(schema: Schema): ValidateFunction;
    validate(schema: Schema, value: unknown, maximumBytes?: number): void;
}
/** Query admission is conservative: structured/ambiguous paths are never implicitly stringified. */
export declare function scalarTypes(schema: Schema, pointer: string): Set<string>;
