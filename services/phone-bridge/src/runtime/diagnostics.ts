import type { Wire } from "../../../../packages/sdk/src/node.js";

export const phoneMicroReady = (state: string): boolean =>
  state === "attached" || state === "ready";

/** Bounded telemetry only. Call ownership and outcomes remain exclusively in PhoneJournal. */
export class PhoneDiagnostics {
  private readonly calls = new Map<string, Wire.Diagnostic>();
  private poll: Wire.Diagnostic | null = null;
  private appTools: Wire.Diagnostic | null = null;
  private registration: Wire.Diagnostic | null = null;
  constructor(
    private readonly serviceNodeId: string,
    private readonly now = () => new Date().toISOString(),
  ) {}
  private observation(
    code: string,
    prior: Wire.Diagnostic | null,
    scope: "call" | "poll" | "registration" | "app_tools",
  ): Wire.Diagnostic {
    const at = this.now();
    return {
      code,
      resource: { serviceNodeId: this.serviceNodeId, scope },
      severity: "warning",
      source: "phone-bridge",
      firstObservedAt: prior?.code === code ? prior.firstObservedAt : at,
      lastObservedAt: at,
      status: "current",
      message:
        scope === "call"
          ? "The original Phone call needs attention. Inspect its retained command outcomes; do not repeat the call."
          : scope === "registration"
            ? "Phone SIP registration or its keepalive needs attention. Inspect phone.status; existing call outcomes remain available."
            : scope === "app_tools"
              ? "Codex App Tools cannot control Voice tasks. SIP remains available; inspect the App Tools error and the installed bundled plugin."
            : "Phone observation is unavailable. Original command outcomes remain available.",
    };
  }
  callFailed(callId: string, code: string): void {
    this.calls.set(
      callId,
      this.observation(code, this.calls.get(callId) ?? null, "call"),
    );
    if (this.calls.size > 32)
      this.calls.delete(this.calls.keys().next().value!);
  }
  /** A later connected call is the recovery proof for failures from earlier calls. Merely
   * releasing a failed call is cleanup, not evidence that the next caller can connect. */
  callSucceeded(_callId: string): void {
    this.calls.clear();
  }
  pollFailed(code: string): void {
    this.poll = this.observation(code, this.poll, "poll");
  }
  pollSucceeded(): void {
    this.poll = null;
  }
  appToolsFailed(code: string): boolean {
    const changed = this.appTools?.code !== code;
    this.appTools = this.observation(code, this.appTools, "app_tools");
    return changed;
  }
  appToolsSucceeded(): boolean {
    const recovered = this.appTools !== null;
    this.appTools = null;
    return recovered;
  }
  registrationObserved(
    enabled: boolean,
    value: Record<string, unknown> | null,
  ): void {
    const code = !enabled
      ? null
      : value?.state !== "registered"
        ? "phone_registration_unavailable"
        : value.lastKeepAliveError
          ? "phone_registration_keepalive_failed"
          : null;
    this.registration = code
      ? this.observation(code, this.registration, "registration")
      : null;
  }
  snapshot(_currentCallId: string | string[] | null): Wire.Diagnostic[] {
    // A successful status read or release cannot prove that Voice startup recovered. Retain
    // the bounded warning until a later call reaches the connected state.
    return structuredClone([
      ...this.calls.values(),
      ...(this.poll ? [this.poll] : []),
      ...(this.appTools ? [this.appTools] : []),
      ...(this.registration ? [this.registration] : []),
    ]);
  }
}
