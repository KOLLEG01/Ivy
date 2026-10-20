import type { Wire } from "../../../../packages/sdk/src/node.js";

/** Bounded telemetry only. Call ownership and outcomes remain exclusively in PhoneJournal. */
export class PhoneDiagnostics {
  private readonly calls = new Map<string, Wire.Diagnostic>();
  private readonly cleanups = new Map<string, Wire.Diagnostic>();
  private readonly observations = new Map<string, Wire.Diagnostic>();
  private poll: Wire.Diagnostic | null = null;
  private voice: Wire.Diagnostic | null = null;
  private registration: Wire.Diagnostic | null = null;
  constructor(
    private readonly serviceNodeId: string,
    private readonly now = () => new Date().toISOString(),
  ) {}
  private observation(
    code: string,
    prior: Wire.Diagnostic | null,
    scope: "call" | "call_cleanup" | "work_observation" | "poll" | "registration" | "codex_voice",
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
        scope === "call_cleanup"
          ? "Phone media cleanup is unconfirmed. Inspect the original stop and release outcomes."
          : scope === "work_observation"
            ? "The original Voice work status is awaiting reconciliation; call media cleanup is independent."
          : scope === "call"
          ? "The original Phone call needs attention. Inspect its retained command outcomes; do not repeat the call."
          : scope === "registration"
            ? "Phone SIP registration or its keepalive needs attention. Inspect phone.status; existing call outcomes remain available."
            : scope === "codex_voice"
              ? "Codex Voice is unavailable. SIP remains available; inspect the configured CLI runtime and its original task outcome."
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
  cleanupFailed(callId: string, code: string): void {
    this.cleanups.set(callId, this.observation(code, this.cleanups.get(callId) ?? null, 'call_cleanup'));
    if (this.cleanups.size > 32) this.cleanups.delete(this.cleanups.keys().next().value!);
  }
  cleanupSucceeded(callId: string): void { this.cleanups.delete(callId); }
  workObserved(callId: string, code: string | null): void {
    if (code === null) this.observations.delete(callId);
    else this.observations.set(callId, this.observation(code, this.observations.get(callId) ?? null, 'work_observation'));
    if (this.observations.size > 64) this.observations.delete(this.observations.keys().next().value!);
  }
  pollFailed(code: string): void {
    this.poll = this.observation(code, this.poll, "poll");
  }
  pollSucceeded(): void {
    this.poll = null;
  }
  voiceFailed(code: string): boolean {
    const changed = this.voice?.code !== code;
    this.voice = this.observation(code, this.voice, "codex_voice");
    return changed;
  }
  voiceSucceeded(): boolean {
    const recovered = this.voice !== null;
    this.voice = null;
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
      ...this.cleanups.values(),
      ...this.observations.values(),
      ...(this.poll ? [this.poll] : []),
      ...(this.voice ? [this.voice] : []),
      ...(this.registration ? [this.registration] : []),
    ]);
  }
}
