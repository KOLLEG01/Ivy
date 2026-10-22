const platforms = new Set(['win32-x64', 'win32-arm64', 'linux-x64', 'linux-arm64']);

/** Native source plans name platform-neutral binaries. Prepared plans name only
 * the selected host's real files; their existing requirements describe that artifact. */
export function materializeNativePlan(plan, { os = process.platform, arch = process.arch } = {}) {
  if (plan.kind === 'app' || !plan.entrypoint?.executable.startsWith('dist/native/')) return plan;
  const platform = `${os}-${arch}`;
  const fail = message => { throw Object.assign(new Error(message), { code: 'invalid_arguments' }); };
  if (!platforms.has(platform)) fail('Native preparation requires a supported host platform.');
  for (const [key, selected] of [['os', os], ['arch', arch]]) {
    const advertised = plan.requirements[key];
    if (advertised !== undefined && !advertised.includes(selected)) fail('The native source plan does not support the selected host.');
  }
  const executable = name => `dist/native/${platform}/${name}${os === 'win32' ? '.exe' : ''}`;
  const select = (command, name) => {
    if (!command || ![`dist/native/${name}`, executable(name)].includes(command.executable)) fail('Native plans must name their service and health binaries directly.');
    return { ...command, executable: executable(name) };
  };
  return {
    ...plan,
    requirements: { ...plan.requirements, os: [os], arch: [arch] },
    entrypoint: select(plan.entrypoint, `ivy-${plan.componentId}`),
    readiness: { ...plan.readiness, command: select(plan.readiness?.command, 'ivy-health') },
  };
}
