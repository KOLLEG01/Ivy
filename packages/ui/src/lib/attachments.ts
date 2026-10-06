export interface AttachmentInfo { name: string; byteLength: number }
export interface UploadedAttachment { src: string; name: string; image: boolean }
export function fileSize(bytes: number): string {
  return bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toLocaleString(undefined, { maximumFractionDigits: 1 })} KiB` : `${(bytes / (1024 * 1024)).toLocaleString(undefined, { maximumFractionDigits: 1 })} MiB`;
}
export function attachmentMarkdown(value: UploadedAttachment): string {
  const label = value.name.replace(/([\\`*_[\]<>])/g, '\\$1').replace(/[\r\n]+/g, ' ');
  return `${value.image ? '!' : ''}[${label}](${value.src})`;
}
