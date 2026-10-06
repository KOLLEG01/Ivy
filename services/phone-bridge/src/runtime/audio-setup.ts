export interface PhoneAudioInventory {
  processLoopbackSupported: boolean;
  devices: { id: string; name: string; direction: 'capture' | 'render'; state: string; muted?: boolean | null }[];
}

/** Current endpoint/mute observations only. This never probes streams or grants Voice authority. */
export function audioSetup(inventory: PhoneAudioInventory, profiles: { route: string; settings: Record<string, unknown> }[]) {
  return { processLoopbackSupported: inventory.processLoopbackSupported, routes: profiles.map(({ route, settings }) => {
    const policy = settings['mutePolicy'] as { mutedRenderEndpointId?: string | null; unmuteReceive?: boolean } | null | undefined;
    const check = (role: string, id: string, direction: 'capture' | 'render', expectedMuted: boolean | null) => {
      const matches = inventory.devices.filter(device => device.id === id), device = matches.length === 1 ? matches[0]! : null;
      const muted = device?.muted ?? null;
      const state = matches.length > 1 ? 'ambiguous' : !device ? 'missing' : device.direction !== direction ? 'wrong_direction' :
        device.state !== 'Active' ? 'inactive' : expectedMuted !== null && muted === null ? 'mute_unknown' :
        expectedMuted !== null && muted !== expectedMuted ? 'mute_mismatch' : 'ready';
      return { role, id, name: device?.name ?? null, state, muted, expectedMuted };
    };
    const mode = settings['sourceMode'];
    const endpoints = [check('receive', String(settings['renderEndpointId']), 'render', policy?.unmuteReceive ? false : null)];
    if (route === 'voice' || mode === 'microphone') endpoints.unshift(check('capture', String(settings['captureEndpointId']), 'capture', null));
    if (mode === 'virtual_speaker') endpoints.unshift(check('loopback', String(settings['loopbackRenderEndpointId']), 'render', null));
    if (policy?.mutedRenderEndpointId) endpoints.push(check('mutedOutput', policy.mutedRenderEndpointId, 'render', true));
    const mutePolicyConflict = policy?.mutedRenderEndpointId === settings['renderEndpointId'];
    return { route, sourceMode: String(settings['sourceMode']), mutePolicyConflict,
      ready: !mutePolicyConflict && !(mode === 'virtual_speaker' && settings['loopbackRenderEndpointId'] === settings['renderEndpointId']) &&
        (mode === 'microphone' || mode === 'virtual_speaker' || inventory.processLoopbackSupported) &&
        endpoints.every(endpoint => endpoint.state === 'ready'), endpoints };
  }) };
}
