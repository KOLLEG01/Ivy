import schema from '../../../specs/schemas/agent.schema.json' with { type: 'json' };
/** Derived from the same checked catalog set as AgentManager's exact public read contract. */
export const nativeVersions: readonly string[] = Object.freeze([...schema.$defs.PreventInput.properties.nativeVersion.enum]);
