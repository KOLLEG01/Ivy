export function submitOnEnter(event: KeyboardEvent, submit: () => void): void {
  if (
    event.key !== "Enter" ||
    event.shiftKey ||
    event.isComposing ||
    event.keyCode === 229
  )
    return;
  event.preventDefault();
  submit();
}

/** A file added to a message draft; `previewUrl` is a local image preview. */
export interface ComposerAttachment {
  id: string;
  name: string;
  previewUrl?: string | null;
  state: "uploading" | "ready" | "failed";
  detail?: string;
}

/** A message waiting for the running turn, as shown in the composer queue. */
export interface QueuedMessage {
  id: string;
  text: string;
  attachments: number;
  sending?: boolean;
  error?: string;
}
