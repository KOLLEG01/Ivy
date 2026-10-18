import browserNotice from "../../../specs/schemas/browser-notice.schema.json" with { type: "json" };

/** Shared envelope for a service-owned *.browser-notification event. */
export interface BrowserNotice {
  title: string;
  body: string;
  tag: string;
  target: { uiId: string; fragment: string };
}
export const browserNoticeSchema = browserNotice;
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
