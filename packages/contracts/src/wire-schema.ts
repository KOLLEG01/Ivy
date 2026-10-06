import wire from '../../../specs/schemas/hive-wire.schema.json' with { type: 'json' };

/** The transport schema is kept as a leaf dependency for small native/host bundles. */
export const wireSchema = wire as Record<string, unknown>;
