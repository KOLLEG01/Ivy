import { randomUUID } from "node:crypto";
import { requireThat } from "../../../../packages/sdk/src/node.js";
import type { PhoneCall } from "./admission.js";
import type { PhoneJournal } from "./journal.js";

export const phoneWorkWindowMs = 300_000;
export interface PhoneVoiceWork {
  threadId: string;
  turnId: string;
  fingerprint: string;
  principalId: string;
  partyKey: string;
  policyHash: string;
  callIds: string[];
  attachedCallId: string | null;
  state:
    | "running"
    | "waiting_input"
    | "completed"
    | "failed"
    | "halting"
    | "paused_deadline"
    | "outcome_unknown";
  endedAt: number | null;
  deadline: number | null;
  revision: number;
  interruptId: string | null;
  error: string | null;
}
export const activePhoneWork = (work: PhoneVoiceWork) =>
  ["running", "waiting_input"].includes(work.state);
export function phoneWorkView(work: PhoneVoiceWork) {
  const {
    threadId,
    turnId,
    state,
    attachedCallId,
    endedAt,
    deadline,
    revision,
    error,
  } = work;
  return {
    threadId,
    turnId,
    state,
    attachedCallId,
    endedAt,
    deadline,
    revision,
    error,
  };
}

/** The call, native turn and deadline have independent, durable identities. */
export class PhoneVoiceWorks {
  private readonly records: Map<string, PhoneVoiceWork>;
  constructor(
    private readonly journal: PhoneJournal,
    readonly now: () => number = Date.now,
  ) {
    const records = journal.voiceWorks();
    requireThat(
      Array.isArray(records) &&
        records.length <= 64 &&
        records.every(
          (work) =>
            typeof work.threadId === "string" &&
            typeof work.turnId === "string" &&
            typeof work.fingerprint === "string" &&
            typeof work.principalId === "string" &&
            typeof work.partyKey === "string" &&
            typeof work.policyHash === "string" &&
            Array.isArray(work.callIds) &&
            work.callIds.length <= 128 &&
            work.callIds.every((id) => typeof id === "string") &&
            [
              "running",
              "waiting_input",
              "completed",
              "failed",
              "halting",
              "paused_deadline",
              "outcome_unknown",
            ].includes(work.state) &&
            Number.isSafeInteger(work.revision) &&
            (work.deadline === null || Number.isFinite(work.deadline)),
        ),
      "phone_work_storage_invalid",
      "The saved Voice work identities are invalid.",
    );
    this.records = new Map(records.map((work) => [work.threadId, work]));
    // A crashed connected transport cannot grant a fresh background allowance.
    for (const work of this.records.values())
      if (activePhoneWork(work) && work.attachedCallId) {
        work.attachedCallId = null;
        work.endedAt = this.now();
        work.deadline = this.now();
        work.revision++;
      }
    this.save();
  }
  private save(): void {
    const terminal = [...this.records.values()].filter((work) =>
      ["completed", "failed"].includes(work.state),
    );
    while (this.records.size > 64 && terminal.length)
      this.records.delete(terminal.shift()!.threadId);
    requireThat(
      this.records.size <= 64,
      "phone_work_capacity",
      "Too many retained Voice work records.",
    );
    this.journal.saveVoiceWorks([...this.records.values()]);
  }
  get(threadId: string): PhoneVoiceWork | undefined {
    return this.records.get(threadId);
  }
  forCall(callId: string): PhoneVoiceWork | undefined {
    return [...this.records.values()]
      .reverse()
      .find((work) => work.callIds.includes(callId));
  }
  protected(threadId: string): boolean {
    const work = this.get(threadId);
    return (
      !!work &&
      (activePhoneWork(work) ||
        ["halting", "paused_deadline", "outcome_unknown"].includes(work.state))
    );
  }
  all(): PhoneVoiceWork[] {
    return [...this.records.values()];
  }
  started(
    call: PhoneCall | undefined,
    threadId: string,
    turnId: string,
    fingerprint: string,
    partyKey: string,
  ): void {
    const existing = this.get(threadId);
    if (existing) {
      if (existing.turnId === turnId) return;
      // A disconnected owner may observe a late turn, but cannot authorize new work.
      const forbidden = !existing.attachedCallId && !call;
      existing.turnId = turnId;
      existing.state = "running";
      existing.error = null;
      existing.interruptId = null;
      if (call) {
        existing.attachedCallId = call.callId;
        if (!existing.callIds.includes(call.callId))
          existing.callIds.push(call.callId);
      }
      existing.callIds = existing.callIds.slice(-128);
      if (forbidden) existing.deadline = this.now();
      existing.revision++;
    } else {
      if (!call) return;
      this.records.set(threadId, {
        threadId,
        turnId,
        fingerprint,
        partyKey,
        principalId: call.principalId,
        policyHash: call.policyHash,
        callIds: [call.callId],
        attachedCallId: call.callId,
        state: "running",
        endedAt: null,
        deadline: null,
        revision: 0,
        interruptId: null,
        error: null,
      });
    }
    this.save();
  }
  completed(threadId: string, turnId: string, status: string): void {
    const work = this.get(threadId);
    if (!work || work.turnId !== turnId) return;
    work.state =
      status === "completed"
        ? "completed"
        : status === "interrupted" && work.interruptId
          ? "paused_deadline"
          : "failed";
    work.revision++;
    this.save();
  }
  waiting(threadId: string): void {
    const work = this.get(threadId);
    if (work?.state === "running") {
      work.state = "waiting_input";
      work.revision++;
      this.save();
    }
  }
  detach(callId: string, endedAt: number | null): PhoneVoiceWork | undefined {
    const work = this.forCall(callId);
    if (!work || work.attachedCallId !== callId) return;
    work.attachedCallId = null;
    work.endedAt = endedAt;
    work.deadline =
      endedAt === null
        ? this.now()
        : Math.min(endedAt, this.now()) + phoneWorkWindowMs;
    work.revision++;
    this.save();
    return work;
  }
  rejoin(
    call: PhoneCall,
    fingerprint: string,
    partyKey: string,
  ): PhoneVoiceWork | undefined {
    const matches = this.all().filter(
      (work) =>
        activePhoneWork(work) &&
        !work.attachedCallId &&
        work.principalId === call.principalId &&
        work.policyHash === call.policyHash &&
        work.fingerprint === fingerprint &&
        work.partyKey === partyKey &&
        work.deadline !== null &&
        this.now() < work.deadline,
    );
    requireThat(
      matches.length <= 1,
      "phone_work_ambiguous",
      "Several running tasks match this caller.",
    );
    return matches[0];
  }
  attach(threadId: string, call: PhoneCall): void {
    const work = this.get(threadId);
    if (!work) return;
    requireThat(
      work.principalId === call.principalId &&
        activePhoneWork(work) &&
        (work.attachedCallId === call.callId ||
          (!work.attachedCallId &&
            work.deadline !== null &&
            this.now() < work.deadline)),
      "phone_work_unavailable",
      "The original work requires outcome reconciliation.",
    );
    work.attachedCallId = call.callId;
    work.endedAt = null;
    work.deadline = null;
    work.revision++;
    if (!work.callIds.includes(call.callId)) work.callIds.push(call.callId);
    work.callIds = work.callIds.slice(-128);
    this.save();
  }
  canAnswer(work: PhoneVoiceWork): boolean {
    return (
      activePhoneWork(work) &&
      (!!work.attachedCallId ||
        (work.deadline !== null && this.now() < work.deadline))
    );
  }
  claimExpired(force = false): PhoneVoiceWork[] {
    const expired = this.all().filter(
      (work) =>
        work.state === "halting" ||
        (activePhoneWork(work) &&
          (force ||
            (!work.attachedCallId &&
              work.deadline !== null &&
              this.now() >= work.deadline))),
    );
    for (const work of expired) {
      if (work.state !== "halting") {
        work.state = "halting";
        work.interruptId ??= randomUUID();
        work.revision++;
      }
    }
    if (expired.length) this.save();
    return expired.map((work) => structuredClone(work));
  }
  halt(callId: string): PhoneVoiceWork | undefined {
    const work = this.forCall(callId);
    if (
      !work ||
      !activePhoneWork(work) ||
      (work.attachedCallId && work.attachedCallId !== callId)
    )
      return;
    work.state = "halting";
    work.interruptId ??= randomUUID();
    work.revision++;
    this.save();
    return structuredClone(work);
  }
  interrupted(claim: PhoneVoiceWork, error: string | null): void {
    const work = this.get(claim.threadId);
    if (
      !work ||
      work.interruptId !== claim.interruptId ||
      work.state !== "halting"
    )
      return;
    // An RPC acknowledgement is not a completed/interrupted turn observation.
    if (error) {
      work.state = "outcome_unknown";
      work.error = error;
      this.save();
    }
  }
  unknown(threadId: string, error: string): void {
    const work = this.get(threadId);
    if (!work) return;
    work.state = "outcome_unknown";
    work.error = error;
    work.attachedCallId = null;
    work.revision++;
    this.save();
  }
}
