/** Shared envelope for a service-owned *.browser-notification event. */
export interface BrowserNotice {
  title: string;
  body: string;
  tag: string;
  target: { uiId: string; fragment: string };
}
export const browserNoticeSchema = {
  type: "object",
  additionalProperties: false,
  properties: {
    title: { type: "string", minLength: 1, maxLength: 100 },
    body: { type: "string", maxLength: 300 },
    tag: { type: "string", minLength: 1, maxLength: 256 },
    target: {
      type: "object",
      additionalProperties: false,
      properties: {
        uiId: { type: "string", pattern: "^[a-z][a-z0-9-]{0,63}$" },
        fragment: { type: "string", pattern: "^#/[ -~]*$", maxLength: 1024 },
      },
      required: ["uiId", "fragment"],
    },
  },
  required: ["title", "body", "tag", "target"],
};
export function browserNoticeTopic(namespace: string) {
  return {
    topic: namespace + ".browser-notification",
    version: "1.0.0",
    title: "Browser notification",
    description: "A user-visible notice for opted-in Ivy browsers.",
    payloadSchema: browserNoticeSchema,
    eventKinds: [],
  };
}
