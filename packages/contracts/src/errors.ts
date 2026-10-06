export type Outcome = "not_executed" | "unknown" | "completed";
export interface WireError {
  code: number;
  message: string;
  data: { code: string; outcome: Outcome; details?: unknown };
}

const statuses: Record<string, number> = {
  unauthenticated: 401,
  forbidden: 403,
  not_found: 404,
  contract_not_found: 404,
  revision_conflict: 409,
  mutation_conflict: 409,
  contract_version_conflict: 409,
  revision_pruned: 410,
  mutation_expired: 410,
  operation_expired: 410,
  contract_definition_conflict: 409,
  contract_owner_mismatch: 409,
  stale_generation: 409,
  target_conflict: 409,
  ambiguous_service_node: 409,
  tool_definition_changed: 409,
  release_conflict: 409,
  subscription_filter_conflict: 409,
  content_too_large: 413,
  frame_too_large: 413,
  result_too_large: 413,
  limit_exceeded: 429,
  capacity_exceeded: 429,
  service_unavailable: 503,
  service_not_ready: 503,
  storage_unavailable: 503,
  deadline_exceeded: 504,
  outcome_unknown: 504,
  internal_error: 500,
  provider_contract_error: 502,
};

export class IvyError extends Error {
  constructor(
    public readonly code: string,
    message: string,
    public readonly outcome: Outcome = "not_executed",
    public readonly details?: unknown,
  ) {
    super(message);
    this.name = "IvyError";
  }
  get httpStatus(): number {
    return statuses[this.code] ?? 400;
  }
  toWire(): WireError {
    return {
      code:
        this.code === "invalid_frame"
          ? -32600
          : this.code === "invalid_arguments"
            ? -32602
            : -32000,
      message: this.message.slice(0, 2048),
      data: {
        code: this.code,
        outcome: this.outcome,
        ...(this.details === undefined ? {} : { details: this.details }),
      },
    };
  }
  static from(error: unknown): IvyError {
    return error instanceof IvyError
      ? error
      : new IvyError(
          "internal_error",
          "The operation failed internally.",
          "unknown",
        );
  }
}

export function fail(
  code: string,
  message: string,
  outcome: Outcome = "not_executed",
): never {
  throw new IvyError(code, message, outcome);
}
export function requireThat(
  value: unknown,
  code: string,
  message: string,
): asserts value {
  if (!value) fail(code, message);
}
