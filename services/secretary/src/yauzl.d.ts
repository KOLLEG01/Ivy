// The used API is pinned to yauzl3.4.0; see its README's Promise/entry/stream contracts.
declare module 'yauzl' {
  import type { Readable } from 'node:stream';
  interface Entry { fileName: string; uncompressedSize: number; crc32: number; isEncrypted(): boolean }
  interface ZipFile { entryCount: number; eachEntry(): AsyncIterable<Entry>; openReadStreamPromise(entry: Entry): Promise<Readable>; close(): void }
  export function fromBufferPromise(bytes: Buffer, options: { validateEntrySizes: boolean; strictFileNames: boolean }): Promise<ZipFile>;
}
