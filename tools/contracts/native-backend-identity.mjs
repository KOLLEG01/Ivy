// The same closed owner identity is used by Agent tools and host maintenance.
export const nativeBackendIdentity = {
  type: "object",
  properties: {
    backendId: { type: "string", pattern: "^[a-z][a-z0-9-]{0,63}$", not: { enum: ["agent"] } },
    provider: { enum: ["codex", "claude"] },
    accountId: { type: "string", pattern: "^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$" },
  },
  required: ["backendId", "provider", "accountId"],
  additionalProperties: false,
};
