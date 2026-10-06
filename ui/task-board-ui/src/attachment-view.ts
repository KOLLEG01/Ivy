export type AttachmentView =
  "markdown" | "image" | "video" | "pdf" | "text" | "download";

export function attachmentView(
  filename: string,
  mediaType: string,
): AttachmentView {
  const type = mediaType.toLowerCase().split(";", 1)[0]!.trim();
  if (
    /\.md$/i.test(filename) ||
    ["text/markdown", "application/markdown", "text/x-markdown"].includes(type)
  )
    return "markdown";
  if (["image/png", "image/jpeg", "image/webp", "image/gif"].includes(type))
    return "image";
  if (["video/mp4", "video/webm", "video/ogg"].includes(type)) return "video";
  if (type === "application/pdf") return "pdf";
  if (type.startsWith("text/") || type === "application/json") return "text";
  return "download";
}
