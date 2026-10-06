import { list, record, text } from "./native";

export interface MessageImage {
  name: string;
  path: string | null;
  url: string | null;
}

/** Preserve native image inputs alongside the message text. */
export function messageImages(content: unknown): MessageImage[] {
  return list(content).flatMap<MessageImage>((part) => {
    const value = record(part);
    if (value.type === "localImage" && text(value.path)) {
      const path = text(value.path);
      return [
        {
          path,
          url: null,
          name: path.split(/[\\/]/).pop() || "Attached image",
        },
      ];
    }
    if (
      value.type === "image" &&
      /^(https?:\/\/|data:image\/(?:png|jpeg|gif|webp|avif);base64,)/i.test(
        text(value.url),
      )
    )
      return [{ path: null, url: text(value.url), name: "Attached image" }];
    return [];
  });
}
