declare module "pngjs" {
  export const PNG: {
    sync: {
      read(bytes: Buffer): { width: number; height: number; data: Buffer };
      write(
        image: { width: number; height: number; data: Buffer },
        options?: {
          bitDepth?: number;
          colorType?: number;
          inputColorType?: number;
        },
      ): Buffer;
    };
  };
}
