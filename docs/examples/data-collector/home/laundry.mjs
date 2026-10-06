// Push input is the appliance's original JSON body, supplied as ctx.input.
export default async function collect({ config, state, signal, input }) {
  signal?.throwIfAborted();
  const device = config.device;
  if (device !== "washer" && device !== "dryer")
    throw new Error("device must be washer or dryer");
  if (!input || typeof input !== "object" || Array.isArray(input))
    throw new Error("Appliance input must be an object");
  if (
    device === "washer" &&
    (!Number.isInteger(input.cycle_st) || input.cycle_st < 0)
  ) {
    throw new Error("Washer input requires numeric cycle_st");
  }
  if (
    device === "dryer" &&
    (!Number.isFinite(input.cycle_timer) || !Number.isFinite(input.step_timer))
  ) {
    throw new Error("Dryer input requires numeric cycle_timer and step_timer");
  }
  if (
    input.tte_showed !== undefined &&
    (!Number.isInteger(input.tte_showed) || input.tte_showed < 0)
  ) {
    throw new Error("Appliance input requires numeric tte_showed");
  }
  const observedAt = new Date().toISOString();
  const sourceTimestamp =
    input.timestamp === undefined
      ? Math.floor(Date.now() / 1000)
      : input.timestamp;
  if (!Number.isFinite(sourceTimestamp) || sourceTimestamp < 0)
    throw new Error("Invalid appliance timestamp");
  const previous = state?.device === device && state?.last ? state.last : null;
  if (previous && sourceTimestamp < previous.sourceTimestamp) {
    return {
      data: { ...previous, ignoredOlderInput: true },
      state,
      events: [],
    };
  }
  const remainingSeconds =
    input.tte_showed === undefined || input.tte_showed === 0xffff
      ? null
      : input.tte_showed;
  const cycleState = device === "washer" ? input.cycle_st : null;
  const data = {
    device,
    observedAt,
    sourceTimestamp,
    cycleState,
    remainingSeconds,
    running: remainingSeconds !== null && remainingSeconds > 0,
    payload: input,
  };
  const events = [];
  const finished =
    previous &&
    (device === "washer"
      ? cycleState === 11 && previous.cycleState !== cycleState
      : remainingSeconds === 0 &&
        previous.remainingSeconds !== null &&
        previous.remainingSeconds > 0);
  if (finished)
    events.push({
      name: "laundry.finished",
      payload: { device, observedAt, sourceTimestamp },
    });
  if (
    (!previous && data.running) ||
    (previous && previous.remainingSeconds !== remainingSeconds)
  ) {
    events.push({
      name: "laundry.status.changed",
      payload: {
        device,
        running: data.running,
        remainingSeconds,
        observedAt,
        sourceTimestamp,
      },
    });
  }
  return { data, state: { device, last: data }, events };
}
