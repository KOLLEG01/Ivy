import type { Host } from '../../contracts/src/generated.js';
export function materializeNativePlan(plan: Host.BuildPlan, platform?: { os?: string; arch?: string }): Host.BuildPlan;
