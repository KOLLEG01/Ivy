/** Windows helpers retained by the component's selected execution path. */
const required = {
  "host-executor": ["dist/native/ivy-job.exe", "dist/native/ivy-host-job.exe"],
  "service-manager": ["dist/native/ivy-job.exe"],
  "agent-manager": ["dist/native/ivy-job.exe"],
  "phone-bridge": ["dist/native/ivy-job.exe", "dist/native/ivy-phone-input.dll"],
} as const;

export function portableNativeFiles(componentId: string, nativeDirectory?: string): readonly string[] {
  const files = required[componentId as keyof typeof required] ?? [];
  return nativeDirectory ? files.map(file => nativeDirectory + '/' + file.slice(file.lastIndexOf('/') + 1)) : files;
}
