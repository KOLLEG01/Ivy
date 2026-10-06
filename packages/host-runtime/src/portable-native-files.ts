/** Windows payloads required in one package distributed to both Linux and Windows hosts. */
const required = {
  "host-executor": ["dist/native/ivy-job.exe", "dist/native/ivy-host-job.exe"],
  "service-manager": ["dist/native/ivy-job.exe"],
  "agent-manager": ["dist/native/ivy-job.exe"],
  "phone-bridge": ["dist/native/ivy-job.exe", "dist/native/ivy-phone-input.dll"],
} as const;

export function portableNativeFiles(componentId: string): readonly string[] {
  return required[componentId as keyof typeof required] ?? [];
}
