import {
  hashJson,
  hive_wire_schema_default
} from "./chunk-PYGN6VSZ.js";
import {
  canonical,
  fail,
  requireThat
} from "./chunk-62CEFMQW.js";

// packages/contracts/src/schema.ts
import { Ajv2020 } from "ajv/dist/2020.js";
import { RE2JS } from "re2js";
var keywords = /* @__PURE__ */ new Set([
  "$schema",
  "$id",
  "$defs",
  "$ref",
  "title",
  "description",
  "default",
  "examples",
  "deprecated",
  "readOnly",
  "writeOnly",
  "format",
  "type",
  "properties",
  "required",
  "additionalProperties",
  "minProperties",
  "maxProperties",
  "items",
  "minItems",
  "maxItems",
  "uniqueItems",
  "enum",
  "const",
  "anyOf",
  "oneOf",
  "allOf",
  "if",
  "then",
  "else",
  "not",
  "minimum",
  "maximum",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "multipleOf",
  "minLength",
  "maxLength",
  "pattern"
]);
function pointerParts(pointer) {
  requireThat(pointer.startsWith("/") && !/~(?:[^01]|$)/.test(pointer), "invalid_arguments", "Invalid JSON pointer.");
  return pointer.slice(1).split("/").map((part) => part.replaceAll("~1", "/").replaceAll("~0", "~"));
}
function resolve(root, reference) {
  requireThat(reference.startsWith("#/"), "registry_invalid", "Only local JSON pointer schema references are supported.");
  let value = root;
  for (const key of pointerParts(reference.slice(1))) {
    requireThat(value !== null && typeof value === "object" && Object.hasOwn(value, key), "registry_invalid", "Unresolved schema reference.");
    value = value[key];
  }
  requireThat(typeof value === "boolean" || value !== null && typeof value === "object" && !Array.isArray(value), "registry_invalid", "Reference must address a schema.");
  return value;
}
function admitSchema(schema2) {
  canonical(schema2, 256 * 1024);
  let visited = 0;
  const inspect = (node, depth, trail, consumption, refs) => {
    requireThat(++visited <= 8192 && depth <= 64, "limit_exceeded", "Schema exceeds the validation complexity limit.");
    if (typeof node === "boolean") return;
    requireThat(node !== null && typeof node === "object" && !Array.isArray(node), "registry_invalid", "Expected a JSON schema.");
    for (const key of Object.keys(node)) requireThat(keywords.has(key), "registry_invalid", `Unsupported schema keyword: ${key.slice(0, 80)}`);
    requireThat(!trail.has(node), "registry_invalid", "Schema has a non-consuming recursive reference.");
    const nextTrail = new Set(trail).add(node);
    if (node["$ref"] !== void 0) {
      requireThat(typeof node["$ref"] === "string", "registry_invalid", "Invalid schema reference.");
      const target = resolve(schema2, node["$ref"]);
      const prior = refs.get(target);
      if (prior !== void 0) requireThat(consumption > prior, "registry_invalid", "Schema recursion must consume a value level.");
      else inspect(target, depth + 1, nextTrail, consumption, new Map(refs).set(target, consumption));
    }
    if (node["pattern"] !== void 0) {
      requireThat(typeof node["pattern"] === "string" && node["pattern"].length <= 1024, "registry_invalid", "Schema pattern exceeds its limit.");
      try {
        RE2JS.compile(node["pattern"]);
      } catch {
        fail("registry_invalid", "Schema pattern is outside the supported RE2 syntax.");
      }
    }
    for (const key of ["anyOf", "oneOf", "allOf"]) if (node[key] !== void 0) {
      requireThat(Array.isArray(node[key]) && node[key].length > 0 && node[key].length <= 32, "registry_invalid", "Schema alternatives exceed their limit.");
      for (const child of node[key]) inspect(child, depth + 1, nextTrail, consumption, refs);
    }
    for (const key of ["if", "then", "else", "not"]) if (node[key] !== void 0) inspect(node[key], depth + 1, nextTrail, consumption, refs);
    for (const key of ["properties", "$defs"]) if (node[key] !== void 0) {
      const map = node[key];
      requireThat(map !== null && typeof map === "object" && !Array.isArray(map), "registry_invalid", "Invalid schema property map.");
      requireThat(Object.keys(map).length <= 1024 && !Object.hasOwn(map, "__proto__"), "registry_invalid", "Unsupported schema property map.");
      for (const child of Object.values(map)) inspect(child, depth + 1, /* @__PURE__ */ new Set(), consumption + (key === "properties" ? 1 : 0), refs);
    }
    for (const key of ["items", "additionalProperties"]) if (node[key] !== void 0) inspect(node[key], depth + 1, /* @__PURE__ */ new Set(), consumption + 1, refs);
  };
  inspect(schema2, 0, /* @__PURE__ */ new Set(), 0, /* @__PURE__ */ new Map([[schema2, 0]]));
}
var re2Engine = Object.assign((pattern, _flags) => {
  const expression = RE2JS.compile(pattern);
  return { test: (input) => expression.matcher(input).find(), toString: () => pattern };
}, { code: "ivyRE2" });
var SchemaValidators = class {
  cache = /* @__PURE__ */ new Map();
  immutable = /* @__PURE__ */ new WeakMap();
  anonymousCompiler = null;
  compilations = 0;
  compile(schema2) {
    const frozen = typeof schema2 === "object" && Object.isFrozen(schema2);
    if (frozen) {
      const validator2 = this.immutable.get(schema2);
      if (validator2) return validator2;
    }
    const identity = hashJson(schema2);
    const existing = this.cache.get(identity);
    if (existing) {
      if (frozen) this.immutable.set(schema2, existing);
      return existing;
    }
    admitSchema(schema2);
    if (this.compilations === 256) {
      this.cache.clear();
      this.anonymousCompiler = null;
      this.compilations = 0;
    }
    this.compilations++;
    const options = {
      strict: false,
      allErrors: false,
      ownProperties: true,
      inlineRefs: false,
      validateFormats: false,
      loopRequired: 64,
      loopEnum: 64,
      code: { regExp: re2Engine }
    };
    const containsId = (value) => value !== null && typeof value === "object" && (Object.hasOwn(value, "$id") || Object.values(value).some(containsId));
    const isolated = containsId(schema2);
    const ajv2 = isolated ? new Ajv2020(options) : this.anonymousCompiler ??= new Ajv2020({ ...options, addUsedSchema: false });
    let validator;
    try {
      validator = ajv2.compile(schema2);
    } catch {
      fail("registry_invalid", "Invalid or unsupported JSON schema.");
    } finally {
      if (!isolated && typeof schema2 === "object") ajv2.removeSchema(schema2);
    }
    this.cache.set(identity, validator);
    if (frozen) this.immutable.set(schema2, validator);
    return validator;
  }
  validate(schema2, value, maximumBytes = 1024 * 1024) {
    canonical(value, maximumBytes);
    requireThat(this.compile(schema2)(value), "invalid_arguments", "Content does not satisfy its exact contract.");
  }
};

// specs/schemas/host.schema.json
var host_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/host.schema.json",
  title: "Host configuration, preparation and durable local deployment protocol",
  $defs: {
    BuildPlan: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        componentId: {
          type: "string",
          pattern: "^[a-z][a-z0-9-]*$",
          maxLength: 128
        },
        kind: {
          enum: [
            "hive",
            "service",
            "service-manager",
            "app",
            "native"
          ]
        },
        version: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        description: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        connectsToHive: {
          type: "boolean"
        },
        requirements: {
          type: "object",
          properties: {
            node: {
              type: "string",
              pattern: "^>=(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*) <(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$",
              maxLength: 128
            },
            os: {
              type: "array",
              items: {
                enum: [
                  "win32",
                  "linux"
                ]
              },
              uniqueItems: true,
              minItems: 1,
              maxItems: 2
            },
            arch: {
              type: "array",
              items: {
                enum: [
                  "x64",
                  "arm64"
                ]
              },
              uniqueItems: true,
              minItems: 1,
              maxItems: 2
            },
            hiveProtocol: {
              anyOf: [
                {
                  const: 1
                },
                {
                  type: "null"
                }
              ]
            },
            contracts: {
              type: "array",
              maxItems: 128,
              items: {
                type: "object",
                properties: {
                  key: {
                    type: "string",
                    minLength: 1,
                    maxLength: 192
                  },
                  readVersions: {
                    type: "array",
                    items: {
                      type: "string",
                      pattern: "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"
                    },
                    uniqueItems: true,
                    maxItems: 64
                  },
                  writeVersions: {
                    type: "array",
                    items: {
                      type: "string",
                      pattern: "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"
                    },
                    uniqueItems: true,
                    maxItems: 64
                  },
                  readScope: {
                    type: "object",
                    properties: {
                      roots: {
                        anyOf: [
                          {
                            type: "null"
                          },
                          {
                            type: "array",
                            items: {
                              type: "string",
                              minLength: 1,
                              maxLength: 256
                            },
                            uniqueItems: true,
                            maxItems: 128
                          }
                        ]
                      },
                      history: {
                        enum: [
                          "current",
                          "all"
                        ]
                      },
                      includeArchived: {
                        type: "boolean"
                      },
                      references: {
                        type: "array",
                        maxItems: 128,
                        uniqueItems: true,
                        items: {
                          type: "object",
                          properties: {
                            objectId: {
                              type: "string",
                              minLength: 1,
                              maxLength: 256
                            },
                            revision: {
                              type: "integer",
                              minimum: 1,
                              maximum: 9007199254740991
                            }
                          },
                          required: [
                            "objectId",
                            "revision"
                          ],
                          additionalProperties: false
                        }
                      }
                    },
                    required: [
                      "roots",
                      "history",
                      "includeArchived",
                      "references"
                    ],
                    additionalProperties: false
                  }
                },
                required: [
                  "key",
                  "readVersions",
                  "writeVersions"
                ],
                additionalProperties: false
              }
            }
          },
          required: [
            "node",
            "hiveProtocol",
            "contracts"
          ],
          additionalProperties: false
        },
        entrypoint: {
          type: "object",
          properties: {
            executable: {
              type: "string",
              minLength: 1,
              maxLength: 1024
            },
            args: {
              type: "array",
              items: {
                type: "string"
              },
              maxItems: 128
            },
            cwd: {
              type: "string",
              minLength: 1,
              maxLength: 1024
            },
            timeoutMs: {
              type: "integer",
              minimum: 1,
              maximum: 36e5
            }
          },
          required: [
            "executable",
            "args",
            "timeoutMs"
          ],
          additionalProperties: false
        },
        prepare: {
          type: "array",
          items: {
            type: "object",
            properties: {
              executable: {
                type: "string",
                minLength: 1,
                maxLength: 1024
              },
              args: {
                type: "array",
                items: {
                  type: "string"
                },
                maxItems: 128
              },
              cwd: {
                type: "string",
                minLength: 1,
                maxLength: 1024
              },
              timeoutMs: {
                type: "integer",
                minimum: 1,
                maximum: 36e5
              }
            },
            required: [
              "executable",
              "args",
              "timeoutMs"
            ],
            additionalProperties: false
          },
          maxItems: 32
        },
        checks: {
          type: "array",
          items: {
            type: "object",
            properties: {
              executable: {
                type: "string",
                minLength: 1,
                maxLength: 1024
              },
              args: {
                type: "array",
                items: {
                  type: "string"
                },
                maxItems: 128
              },
              cwd: {
                type: "string",
                minLength: 1,
                maxLength: 1024
              },
              timeoutMs: {
                type: "integer",
                minimum: 1,
                maximum: 36e5
              }
            },
            required: [
              "executable",
              "args",
              "timeoutMs"
            ],
            additionalProperties: false
          },
          maxItems: 32
        },
        readiness: {
          type: "object",
          properties: {
            timeoutMs: {
              type: "integer",
              minimum: 1,
              maximum: 6e5
            },
            command: {
              type: "object",
              properties: {
                executable: {
                  type: "string",
                  minLength: 1,
                  maxLength: 1024
                },
                args: {
                  type: "array",
                  items: {
                    type: "string"
                  },
                  maxItems: 128
                },
                cwd: {
                  type: "string",
                  minLength: 1,
                  maxLength: 1024
                },
                timeoutMs: {
                  type: "integer",
                  minimum: 1,
                  maximum: 36e5
                }
              },
              required: [
                "executable",
                "args",
                "timeoutMs"
              ],
              additionalProperties: false
            }
          },
          required: [
            "timeoutMs",
            "command"
          ],
          additionalProperties: false
        },
        shutdown: {
          type: "object",
          properties: {
            timeoutMs: {
              type: "integer",
              minimum: 1,
              maximum: 6e5
            }
          },
          required: [
            "timeoutMs"
          ],
          additionalProperties: false
        },
        restart: {
          type: "object",
          properties: {
            policy: {
              enum: [
                "never",
                "on-failure",
                "always"
              ]
            },
            minimumDelayMs: {
              type: "integer",
              minimum: 100,
              maximum: 6e5
            },
            maximumDelayMs: {
              type: "integer",
              minimum: 100,
              maximum: 36e5
            }
          },
          required: [
            "policy",
            "minimumDelayMs",
            "maximumDelayMs"
          ],
          additionalProperties: false
        },
        app: {
          type: "object",
          properties: {
            appId: {
              type: "string",
              pattern: "^[a-z][a-z0-9-]*$",
              maxLength: 128
            },
            dist: {
              type: "string",
              pattern: "^dist/apps/[a-z][a-z0-9-]*$",
              maxLength: 1024
            },
            entryPath: {
              type: "string",
              pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+\\.html$",
              maxLength: 1024
            }
          },
          required: [
            "appId",
            "dist",
            "entryPath"
          ],
          additionalProperties: false
        },
        storage: {
          type: "object",
          properties: {
            minReadableFormat: {
              type: "integer",
              minimum: 1
            },
            maxReadableFormat: {
              type: "integer",
              minimum: 1
            },
            writeFormat: {
              type: "integer",
              minimum: 1
            }
          },
          required: [
            "minReadableFormat",
            "maxReadableFormat",
            "writeFormat"
          ],
          additionalProperties: false
        },
        runtimeReset: {
          type: "object",
          properties: {
            delete: {
              type: "array",
              maxItems: 64,
              items: {
                type: "object",
                properties: {
                  area: {
                    enum: [
                      "host",
                      "data",
                      "work",
                      "logs"
                    ]
                  },
                  path: {
                    type: "string",
                    minLength: 1,
                    maxLength: 256,
                    pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$"
                  }
                },
                required: [
                  "area",
                  "path"
                ],
                additionalProperties: false
              }
            },
            preserve: {
              type: "array",
              maxItems: 64,
              items: {
                type: "object",
                properties: {
                  area: {
                    enum: [
                      "host",
                      "data",
                      "work",
                      "logs"
                    ]
                  },
                  path: {
                    type: "string",
                    minLength: 1,
                    maxLength: 256,
                    pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$"
                  }
                },
                required: [
                  "area",
                  "path"
                ],
                additionalProperties: false
              }
            },
            completionMarker: {
              type: "string",
              minLength: 1,
              maxLength: 256,
              pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$"
            }
          },
          required: [
            "delete",
            "preserve"
          ],
          additionalProperties: false
        }
      },
      required: [
        "schemaVersion",
        "componentId",
        "kind",
        "version",
        "description",
        "connectsToHive",
        "requirements",
        "prepare",
        "checks"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              kind: {
                const: "app"
              }
            },
            required: [
              "kind"
            ]
          },
          then: {
            required: [
              "app"
            ],
            properties: {
              connectsToHive: {
                const: false
              }
            },
            not: {
              required: [
                "entrypoint"
              ]
            }
          },
          else: {
            required: [
              "entrypoint",
              "readiness",
              "shutdown",
              "restart"
            ]
          }
        },
        {
          if: {
            properties: {
              connectsToHive: {
                const: true
              }
            },
            required: [
              "connectsToHive"
            ]
          },
          then: {
            properties: {
              requirements: {
                properties: {
                  hiveProtocol: {
                    const: 1
                  }
                }
              }
            }
          }
        },
        {
          if: {
            properties: {
              kind: {
                const: "hive"
              }
            },
            required: [
              "kind"
            ]
          },
          then: {
            required: [
              "storage"
            ],
            properties: {
              connectsToHive: {
                const: false
              }
            }
          }
        }
      ]
    },
    ReleaseManifest: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        componentId: {
          type: "string",
          pattern: "^[a-z][a-z0-9-]*$",
          maxLength: 128
        },
        kind: {
          enum: [
            "hive",
            "service",
            "service-manager",
            "app",
            "native"
          ]
        },
        version: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        connectsToHive: {
          type: "boolean"
        },
        requirements: {
          type: "object",
          properties: {
            node: {
              type: "string",
              pattern: "^>=(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*) <(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$",
              maxLength: 128
            },
            os: {
              type: "array",
              items: {
                enum: [
                  "win32",
                  "linux"
                ]
              },
              uniqueItems: true,
              minItems: 1,
              maxItems: 2
            },
            arch: {
              type: "array",
              items: {
                enum: [
                  "x64",
                  "arm64"
                ]
              },
              uniqueItems: true,
              minItems: 1,
              maxItems: 2
            },
            hiveProtocol: {
              anyOf: [
                {
                  const: 1
                },
                {
                  type: "null"
                }
              ]
            },
            contracts: {
              type: "array",
              maxItems: 128,
              items: {
                type: "object",
                properties: {
                  key: {
                    type: "string",
                    minLength: 1,
                    maxLength: 192
                  },
                  readVersions: {
                    type: "array",
                    items: {
                      type: "string",
                      pattern: "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"
                    },
                    uniqueItems: true,
                    maxItems: 64
                  },
                  writeVersions: {
                    type: "array",
                    items: {
                      type: "string",
                      pattern: "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"
                    },
                    uniqueItems: true,
                    maxItems: 64
                  },
                  readScope: {
                    type: "object",
                    properties: {
                      roots: {
                        anyOf: [
                          {
                            type: "null"
                          },
                          {
                            type: "array",
                            items: {
                              type: "string",
                              minLength: 1,
                              maxLength: 256
                            },
                            uniqueItems: true,
                            maxItems: 128
                          }
                        ]
                      },
                      history: {
                        enum: [
                          "current",
                          "all"
                        ]
                      },
                      includeArchived: {
                        type: "boolean"
                      },
                      references: {
                        type: "array",
                        maxItems: 128,
                        uniqueItems: true,
                        items: {
                          type: "object",
                          properties: {
                            objectId: {
                              type: "string",
                              minLength: 1,
                              maxLength: 256
                            },
                            revision: {
                              type: "integer",
                              minimum: 1,
                              maximum: 9007199254740991
                            }
                          },
                          required: [
                            "objectId",
                            "revision"
                          ],
                          additionalProperties: false
                        }
                      }
                    },
                    required: [
                      "roots",
                      "history",
                      "includeArchived",
                      "references"
                    ],
                    additionalProperties: false
                  }
                },
                required: [
                  "key",
                  "readVersions",
                  "writeVersions"
                ],
                additionalProperties: false
              }
            }
          },
          required: [
            "node",
            "hiveProtocol",
            "contracts"
          ],
          additionalProperties: false
        },
        entrypoint: {
          type: "object",
          properties: {
            executable: {
              type: "string",
              minLength: 1,
              maxLength: 1024
            },
            args: {
              type: "array",
              items: {
                type: "string"
              },
              maxItems: 128
            },
            cwd: {
              type: "string",
              minLength: 1,
              maxLength: 1024
            },
            timeoutMs: {
              type: "integer",
              minimum: 1,
              maximum: 36e5
            }
          },
          required: [
            "executable",
            "args",
            "timeoutMs"
          ],
          additionalProperties: false
        },
        readiness: {
          type: "object",
          properties: {
            timeoutMs: {
              type: "integer",
              minimum: 1,
              maximum: 6e5
            },
            command: {
              type: "object",
              properties: {
                executable: {
                  type: "string",
                  minLength: 1,
                  maxLength: 1024
                },
                args: {
                  type: "array",
                  items: {
                    type: "string"
                  },
                  maxItems: 128
                },
                cwd: {
                  type: "string",
                  minLength: 1,
                  maxLength: 1024
                },
                timeoutMs: {
                  type: "integer",
                  minimum: 1,
                  maximum: 36e5
                }
              },
              required: [
                "executable",
                "args",
                "timeoutMs"
              ],
              additionalProperties: false
            }
          },
          required: [
            "timeoutMs",
            "command"
          ],
          additionalProperties: false
        },
        shutdown: {
          type: "object",
          properties: {
            timeoutMs: {
              type: "integer",
              minimum: 1,
              maximum: 6e5
            }
          },
          required: [
            "timeoutMs"
          ],
          additionalProperties: false
        },
        restart: {
          type: "object",
          properties: {
            policy: {
              enum: [
                "never",
                "on-failure",
                "always"
              ]
            },
            minimumDelayMs: {
              type: "integer",
              minimum: 100,
              maximum: 6e5
            },
            maximumDelayMs: {
              type: "integer",
              minimum: 100,
              maximum: 36e5
            }
          },
          required: [
            "policy",
            "minimumDelayMs",
            "maximumDelayMs"
          ],
          additionalProperties: false
        },
        app: {
          type: "object",
          properties: {
            appId: {
              type: "string",
              pattern: "^[a-z][a-z0-9-]*$",
              maxLength: 128
            },
            dist: {
              type: "string",
              pattern: "^dist/apps/[a-z][a-z0-9-]*$",
              maxLength: 1024
            },
            entryPath: {
              type: "string",
              pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+\\.html$",
              maxLength: 1024
            }
          },
          required: [
            "appId",
            "dist",
            "entryPath"
          ],
          additionalProperties: false
        },
        storage: {
          type: "object",
          properties: {
            minReadableFormat: {
              type: "integer",
              minimum: 1
            },
            maxReadableFormat: {
              type: "integer",
              minimum: 1
            },
            writeFormat: {
              type: "integer",
              minimum: 1
            }
          },
          required: [
            "minReadableFormat",
            "maxReadableFormat",
            "writeFormat"
          ],
          additionalProperties: false
        },
        runtimeReset: {
          type: "object",
          properties: {
            delete: {
              type: "array",
              maxItems: 64,
              items: {
                type: "object",
                properties: {
                  area: {
                    enum: [
                      "host",
                      "data",
                      "work",
                      "logs"
                    ]
                  },
                  path: {
                    type: "string",
                    minLength: 1,
                    maxLength: 256,
                    pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$"
                  }
                },
                required: [
                  "area",
                  "path"
                ],
                additionalProperties: false
              }
            },
            preserve: {
              type: "array",
              maxItems: 64,
              items: {
                type: "object",
                properties: {
                  area: {
                    enum: [
                      "host",
                      "data",
                      "work",
                      "logs"
                    ]
                  },
                  path: {
                    type: "string",
                    minLength: 1,
                    maxLength: 256,
                    pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$"
                  }
                },
                required: [
                  "area",
                  "path"
                ],
                additionalProperties: false
              }
            },
            completionMarker: {
              type: "string",
              minLength: 1,
              maxLength: 256,
              pattern: "^(?![\\\\/])(?!.*(?:^|[\\\\/])\\.\\.(?:[\\\\/]|$)).+$"
            }
          },
          required: [
            "delete",
            "preserve"
          ],
          additionalProperties: false
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "schemaVersion",
        "componentId",
        "kind",
        "version",
        "buildId",
        "connectsToHive",
        "requirements"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              kind: {
                const: "app"
              }
            },
            required: [
              "kind"
            ]
          },
          then: {
            required: [
              "app"
            ],
            properties: {
              connectsToHive: {
                const: false
              }
            },
            not: {
              required: [
                "entrypoint"
              ]
            }
          },
          else: {
            required: [
              "entrypoint",
              "readiness",
              "shutdown",
              "restart"
            ]
          }
        },
        {
          if: {
            properties: {
              connectsToHive: {
                const: true
              }
            },
            required: [
              "connectsToHive"
            ]
          },
          then: {
            properties: {
              requirements: {
                properties: {
                  hiveProtocol: {
                    const: 1
                  }
                }
              }
            }
          }
        },
        {
          if: {
            properties: {
              kind: {
                const: "hive"
              }
            },
            required: [
              "kind"
            ]
          },
          then: {
            required: [
              "storage"
            ],
            properties: {
              connectsToHive: {
                const: false
              }
            }
          }
        }
      ]
    },
    Credential: {
      type: "object",
      properties: {
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        token: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        }
      },
      required: [
        "principalId",
        "token"
      ],
      additionalProperties: false
    },
    HiveSettings: {
      type: "object",
      properties: {
        listenHost: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        listenPort: {
          type: "integer",
          minimum: 1,
          maximum: 65535
        },
        credentials: {
          type: "array",
          items: {
            $ref: "#/$defs/Credential"
          },
          maxItems: 128
        },
        packagePublisherPrincipalIds: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 128
          },
          maxItems: 128
        },
        backup: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              minLength: 1,
              maxLength: 2048
            },
            intervalHours: {
              type: "integer",
              minimum: 1,
              maximum: 168
            },
            retain: {
              type: "integer",
              minimum: 1,
              maximum: 365
            }
          },
          required: [
            "directory",
            "intervalHours",
            "retain"
          ],
          additionalProperties: false
        },
        trustedProxyAddresses: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 64
          },
          maxItems: 32
        },
        debug: {
          type: "object",
          properties: {
            validateMessages: {
              type: "boolean"
            }
          },
          required: [],
          additionalProperties: false
        }
      },
      required: [
        "listenHost",
        "listenPort",
        "credentials",
        "backup"
      ],
      additionalProperties: false
    },
    InstanceConfig: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        componentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        publicBaseUrl: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        dataRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        workRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        logsRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        artifactRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        version: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        credential: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        },
        settings: {
          type: "object",
          additionalProperties: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
          }
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "instanceId",
        "serviceNodeId",
        "componentId",
        "publicBaseUrl",
        "dataRoot",
        "artifactRoot",
        "buildId",
        "version",
        "settings"
      ],
      additionalProperties: false
    },
    Instance: {
      type: "object",
      properties: {
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        componentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        enabled: {
          type: "boolean"
        },
        engine: {
          enum: [
            "process",
            "docker"
          ]
        },
        credential: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        },
        secretPaths: {
          type: "array",
          items: {
            type: "string",
            pattern: "^/settings(?:/(?![0-9]+(?:/|$))(?:[^~/]|~[01])*)+$",
            maxLength: 2048,
            description: "Explicit setting secret. Protect an array at its containing path; numeric element paths are forbidden."
          },
          maxItems: 64,
          uniqueItems: true
        },
        paths: {
          type: "object",
          properties: {
            data: {
              type: "string",
              minLength: 1,
              maxLength: 2048
            },
            work: {
              type: "string",
              minLength: 1,
              maxLength: 2048
            },
            logs: {
              type: "string",
              minLength: 1,
              maxLength: 2048
            }
          },
          required: [
            "data",
            "work",
            "logs"
          ],
          additionalProperties: false
        },
        settings: {
          type: "object",
          additionalProperties: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
          }
        },
        process: {
          type: "object",
          properties: {
            allowWindowsBreakaway: {
              type: "boolean"
            }
          },
          required: [
            "allowWindowsBreakaway"
          ],
          additionalProperties: false
        },
        docker: {
          type: "object",
          properties: {
            imageRepository: {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            ports: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  host: {
                    type: "string",
                    minLength: 1,
                    maxLength: 256
                  },
                  port: {
                    type: "integer",
                    minimum: 1,
                    maximum: 65535
                  },
                  containerPort: {
                    type: "integer",
                    minimum: 1,
                    maximum: 65535
                  }
                },
                required: [
                  "host",
                  "port",
                  "containerPort"
                ],
                additionalProperties: false
              },
              maxItems: 16
            }
          },
          required: [
            "imageRepository",
            "ports"
          ],
          additionalProperties: false
        }
      },
      required: [
        "instanceId",
        "serviceNodeId",
        "componentId",
        "enabled",
        "engine",
        "settings"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              engine: {
                const: "docker"
              }
            },
            required: [
              "engine"
            ]
          },
          then: {
            not: {
              required: [
                "process"
              ]
            }
          }
        }
      ]
    },
    HostConfig: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        developmentMode: {
          type: "boolean"
        },
        ivyRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        servicesRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        configPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        runtimeRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        artifactRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        stagingRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        publicBaseUrl: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        executables: {
          type: "object",
          minProperties: 1,
          maxProperties: 32,
          additionalProperties: {
            type: "string",
            minLength: 1,
            maxLength: 2048
          },
          properties: {
            dotnet: {
              type: "string",
              minLength: 1,
              maxLength: 2048,
              description: "Optional absolute .NET SDK executable for nested preparation/check commands; forwarded as IVY_DOTNET."
            },
            gcc: {
              type: "string",
              minLength: 1,
              maxLength: 2048,
              description: "Optional absolute MinGW C compiler for nested preparation/check commands; forwarded as IVY_EVS_CC."
            }
          }
        },
        instances: {
          type: "array",
          items: {
            $ref: "#/$defs/Instance"
          },
          maxItems: 128
        },
        deployment: {
          type: "object",
          properties: {
            identity: {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            hosts: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  hostId: {
                    type: "string",
                    minLength: 1,
                    maxLength: 128
                  },
                  configPath: {
                    type: "string",
                    minLength: 1,
                    maxLength: 2048
                  },
                  ssh: {
                    type: "string",
                    minLength: 1,
                    maxLength: 256
                  },
                  cliPath: {
                    type: "string",
                    minLength: 1,
                    maxLength: 2048
                  }
                },
                required: [
                  "hostId",
                  "configPath"
                ],
                additionalProperties: false
              },
              maxItems: 32
            }
          },
          required: [
            "identity",
            "hosts"
          ],
          additionalProperties: false
        },
        configurationUpdates: {
          type: "object",
          properties: {
            intervalSeconds: {
              type: "integer",
              minimum: 2,
              maximum: 3600
            }
          },
          required: [
            "intervalSeconds"
          ],
          additionalProperties: false
        },
        packageUpdates: {
          type: "object",
          properties: {
            intervalSeconds: {
              type: "integer",
              minimum: 2,
              maximum: 3600
            }
          },
          required: [
            "intervalSeconds"
          ],
          additionalProperties: false
        },
        restoredFrom: {
          type: "object",
          properties: {
            backupId: {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            manifestHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            restoreId: {
              type: "string",
              minLength: 1,
              maxLength: 128
            }
          },
          required: [
            "backupId",
            "manifestHash",
            "restoreId"
          ],
          additionalProperties: false
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "runtimeRoot",
        "artifactRoot",
        "stagingRoot",
        "publicBaseUrl",
        "executables",
        "instances"
      ],
      additionalProperties: false
    },
    LocalRequest: {
      type: "object",
      properties: {
        action: {
          enum: [
            "deploy",
            "rollback",
            "restart",
            "enable",
            "disable"
          ]
        },
        operationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        candidateId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        source: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        componentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        targetBuild: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "action",
        "operationId",
        "instanceId"
      ],
      additionalProperties: false
    },
    SourceSnapshot: {
      type: "object",
      properties: {
        snapshotId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        sourceRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        originalRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        manifestPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        capturedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        }
      },
      required: [
        "snapshotId",
        "sourceRoot",
        "originalRoot",
        "manifestPath",
        "capturedAt"
      ],
      additionalProperties: false
    },
    FileEntry: {
      type: "object",
      properties: {
        path: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        hash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        bytes: {
          type: "integer",
          minimum: 0,
          maximum: 2147483648
        },
        mode: {
          type: "integer",
          minimum: 0,
          maximum: 511
        }
      },
      required: [
        "path",
        "hash",
        "bytes",
        "mode"
      ],
      additionalProperties: false
    },
    SnapshotManifest: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        snapshot: {
          $ref: "#/$defs/SourceSnapshot"
        }
      },
      required: [
        "schemaVersion",
        "snapshot"
      ],
      additionalProperties: false
    },
    ArtifactFiles: {
      type: "array",
      items: {
        $ref: "#/$defs/FileEntry"
      },
      maxItems: 5e4
    },
    JournalEntry: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        request: {
          $ref: "#/$defs/LocalRequest"
        },
        record: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DeploymentRecord"
        },
        candidateId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        previousCandidateId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        configurationPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        sourceSnapshot: {
          $ref: "#/$defs/SourceSnapshot"
        },
        activation: {
          type: "object",
          properties: {
            previousTarget: {
              anyOf: [
                {
                  $ref: "#/$defs/RuntimeTarget"
                },
                {
                  type: "null"
                }
              ]
            },
            previousConfigurationPath: {
              type: "string",
              minLength: 1,
              maxLength: 2048
            },
            previousEnabled: {
              type: "boolean"
            },
            targetEnabled: {
              type: "boolean"
            },
            rollbackTargetRevision: {
              anyOf: [
                {
                  type: "string",
                  minLength: 1,
                  maxLength: 128
                },
                {
                  type: "null"
                }
              ]
            }
          },
          required: [
            "previousTarget",
            "previousEnabled",
            "targetEnabled",
            "rollbackTargetRevision"
          ],
          additionalProperties: false
        }
      },
      required: [
        "schemaVersion",
        "request",
        "record",
        "candidateId",
        "previousCandidateId",
        "configurationPath",
        "activation"
      ],
      additionalProperties: false
    },
    Candidate: {
      type: "object",
      properties: {
        candidateId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        componentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        artifactRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        manifestPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        createdAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        platform: {
          type: "object",
          properties: {
            os: {
              enum: [
                "win32",
                "linux"
              ]
            },
            arch: {
              enum: [
                "x64",
                "arm64"
              ]
            },
            node: {
              type: "string",
              minLength: 1,
              maxLength: 128
            }
          },
          required: [
            "os",
            "arch",
            "node"
          ],
          additionalProperties: false
        },
        archivePath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        archiveHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        dockerImage: {
          type: "string",
          minLength: 1,
          maxLength: 512
        }
      },
      required: [
        "candidateId",
        "componentId",
        "artifactRoot",
        "manifestPath",
        "createdAt",
        "platform",
        "buildId"
      ],
      additionalProperties: false
    },
    PreparationRecord: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        preparationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        componentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        snapshot: {
          $ref: "#/$defs/SourceSnapshot"
        },
        phase: {
          enum: [
            "building",
            "publishing",
            "verified",
            "failed",
            "unknown",
            "compacted"
          ]
        },
        candidate: {
          anyOf: [
            {
              $ref: "#/$defs/Candidate"
            },
            {
              type: "null"
            }
          ]
        },
        startedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        updatedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        errorCode: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "preparationId",
        "componentId",
        "buildId",
        "snapshot",
        "phase",
        "candidate",
        "startedAt",
        "updatedAt",
        "errorCode"
      ],
      additionalProperties: false
    },
    PreparationInventory: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        availableBytes: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        entries: {
          type: "array",
          items: {
            type: "object",
            properties: {
              preparationId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              path: {
                type: "string",
                minLength: 1,
                maxLength: 2048
              },
              phase: {
                enum: [
                  "building",
                  "publishing",
                  "verified",
                  "failed",
                  "unknown",
                  "compacted",
                  "unrecognized"
                ]
              },
              buildId: {
                anyOf: [
                  {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
                  },
                  {
                    type: "null"
                  }
                ]
              },
              candidateId: {
                anyOf: [
                  {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
                  },
                  {
                    type: "null"
                  }
                ]
              },
              bytes: {
                type: "integer",
                minimum: 0,
                maximum: 9007199254740991
              },
              code: {
                anyOf: [
                  {
                    type: "string",
                    minLength: 1,
                    maxLength: 128
                  },
                  {
                    type: "null"
                  }
                ]
              }
            },
            required: [
              "preparationId",
              "path",
              "phase",
              "buildId",
              "candidateId",
              "bytes",
              "code"
            ],
            additionalProperties: false
          },
          maxItems: 1e4
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "observedAt",
        "availableBytes",
        "entries"
      ],
      additionalProperties: false
    },
    PreparationCompaction: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        entries: {
          type: "array",
          items: {
            type: "object",
            properties: {
              preparationId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              action: {
                enum: [
                  "compacted",
                  "recovered",
                  "retained",
                  "busy"
                ]
              },
              candidateId: {
                anyOf: [
                  {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
                  },
                  {
                    type: "null"
                  }
                ]
              },
              reclaimedBytes: {
                type: "integer",
                minimum: 0,
                maximum: 9007199254740991
              },
              code: {
                anyOf: [
                  {
                    type: "string",
                    minLength: 1,
                    maxLength: 128
                  },
                  {
                    type: "null"
                  }
                ]
              }
            },
            required: [
              "preparationId",
              "action",
              "candidateId",
              "reclaimedBytes",
              "code"
            ],
            additionalProperties: false
          },
          maxItems: 1e4
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "observedAt",
        "entries"
      ],
      additionalProperties: false
    },
    BackupFile: {
      type: "object",
      properties: {
        path: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        hash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        bytes: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        mode: {
          type: "integer",
          minimum: 0,
          maximum: 511
        }
      },
      required: [
        "path",
        "hash",
        "bytes",
        "mode"
      ],
      additionalProperties: false
    },
    BackupManifest: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        backupId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        createdAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        completedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        consistency: {
          const: "offline-owned-state"
        },
        platform: {
          type: "object",
          properties: {
            os: {
              enum: [
                "win32",
                "linux"
              ]
            },
            arch: {
              type: "string",
              minLength: 1,
              maxLength: 64
            },
            node: {
              type: "string",
              minLength: 1,
              maxLength: 128
            }
          },
          required: [
            "os",
            "arch",
            "node"
          ],
          additionalProperties: false
        },
        configurationHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        files: {
          type: "array",
          items: {
            $ref: "#/$defs/BackupFile"
          },
          maxItems: 5e4
        },
        externalFiles: {
          type: "array",
          items: {
            $ref: "#/$defs/BackupFile"
          },
          maxItems: 64
        },
        hiveSnapshots: {
          type: "array",
          items: {
            type: "object",
            properties: {
              instanceId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              path: {
                type: "string",
                minLength: 1,
                maxLength: 2048
              },
              format: {
                type: "integer",
                minimum: 1,
                maximum: 9007199254740991
              },
              pages: {
                type: "integer",
                minimum: 1,
                maximum: 9007199254740991
              }
            },
            required: [
              "instanceId",
              "path",
              "format",
              "pages"
            ],
            additionalProperties: false
          },
          maxItems: 128
        },
        excluded: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 256
          },
          maxItems: 64
        },
        ephemeralNativePaths: {
          type: "array",
          items: {
            type: "object",
            properties: {
              instanceId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              path: {
                type: "string",
                minLength: 1,
                maxLength: 2048
              },
              kind: {
                const: "codex-arg0-helper-cache"
              }
            },
            required: [
              "instanceId",
              "path",
              "kind"
            ],
            additionalProperties: false
          },
          maxItems: 128
        },
        sharedNativeHomes: {
          type: "array",
          items: {
            type: "object",
            properties: {
              instanceId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              path: {
                type: "string",
                minLength: 1,
                maxLength: 2048
              },
              kind: {
                const: "external-user-state"
              }
            },
            required: [
              "instanceId",
              "path",
              "kind"
            ],
            additionalProperties: false
          },
          maxItems: 128
        },
        storageAreas: {
          type: "array",
          items: {
            type: "object",
            properties: {
              key: {
                type: "string",
                minLength: 1,
                maxLength: 512
              },
              path: {
                type: "string",
                minLength: 1,
                maxLength: 2048
              }
            },
            required: [
              "key",
              "path"
            ],
            additionalProperties: false
          },
          maxItems: 512
        }
      },
      required: [
        "schemaVersion",
        "backupId",
        "hostId",
        "createdAt",
        "completedAt",
        "consistency",
        "platform",
        "configurationHash",
        "files",
        "externalFiles",
        "hiveSnapshots",
        "excluded"
      ],
      additionalProperties: false
    },
    BackupResult: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        backupId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        directory: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        manifestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        completedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        files: {
          type: "integer",
          minimum: 1,
          maximum: 5e4
        },
        bytes: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        elapsedMs: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "schemaVersion",
        "backupId",
        "hostId",
        "directory",
        "manifestHash",
        "completedAt",
        "files",
        "bytes",
        "elapsedMs"
      ],
      additionalProperties: false
    },
    NativeStorageLayout: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        nativeVersion: {
          enum: [
            "0.154.0"
          ]
        },
        database: {
          const: "state_5.sqlite"
        },
        upstream: {
          type: "object",
          properties: {
            tag: {
              type: "string",
              minLength: 1,
              maxLength: 64
            },
            tree: {
              type: "string",
              pattern: "^[a-f0-9]{40}$"
            },
            migrationLineEndings: {
              enum: [
                "lf",
                "crlf"
              ]
            },
            source: {
              type: "string",
              minLength: 1,
              maxLength: 2048
            }
          },
          required: [
            "tag",
            "tree",
            "migrationLineEndings",
            "source"
          ],
          additionalProperties: false
        },
        migrations: {
          type: "array",
          items: {
            type: "object",
            properties: {
              version: {
                type: "integer",
                minimum: 1,
                maximum: 1e3
              },
              description: {
                type: "string",
                minLength: 1,
                maxLength: 256
              },
              checksum: {
                type: "string",
                pattern: "^[a-f0-9]{96}$"
              }
            },
            required: [
              "version",
              "description",
              "checksum"
            ],
            additionalProperties: false
          },
          maxItems: 1e3
        },
        schema: {
          type: "array",
          items: {
            type: "object",
            properties: {
              type: {
                enum: [
                  "table",
                  "index",
                  "trigger",
                  "view"
                ]
              },
              name: {
                type: "string",
                minLength: 1,
                maxLength: 256
              },
              tableName: {
                type: "string",
                minLength: 1,
                maxLength: 256
              },
              sql: {
                type: "string",
                minLength: 1,
                maxLength: 65536
              }
            },
            required: [
              "type",
              "name",
              "tableName",
              "sql"
            ],
            additionalProperties: false
          },
          maxItems: 1e3
        },
        pathColumns: {
          type: "array",
          items: {
            type: "object",
            properties: {
              table: {
                type: "string",
                minLength: 1,
                maxLength: 256
              },
              column: {
                type: "string",
                minLength: 1,
                maxLength: 256
              },
              scope: {
                enum: [
                  "required-rollout",
                  "rollout-marker",
                  "optional-owned-file"
                ]
              }
            },
            required: [
              "table",
              "column",
              "scope"
            ],
            additionalProperties: false
          },
          maxItems: 8
        }
      },
      required: [
        "schemaVersion",
        "nativeVersion",
        "database",
        "upstream",
        "migrations",
        "schema",
        "pathColumns"
      ],
      additionalProperties: false
    },
    NativeRecoveryReport: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        restoreId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        backupId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        manifestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        completedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        instances: {
          type: "array",
          items: {
            type: "object",
            properties: {
              instanceId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              nativeVersion: {
                type: "string",
                minLength: 1,
                maxLength: 64
              },
              layoutHash: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              database: {
                const: "state_5.sqlite"
              },
              originalFiles: {
                type: "array",
                items: {
                  $ref: "#/$defs/BackupFile"
                },
                maxItems: 3
              },
              workingHash: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              logicalDataHash: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              changes: {
                type: "array",
                items: {
                  type: "object",
                  properties: {
                    table: {
                      type: "string",
                      minLength: 1,
                      maxLength: 256
                    },
                    column: {
                      type: "string",
                      minLength: 1,
                      maxLength: 256
                    },
                    rows: {
                      type: "integer",
                      minimum: 0,
                      maximum: 1e6
                    }
                  },
                  required: [
                    "table",
                    "column",
                    "rows"
                  ],
                  additionalProperties: false
                },
                maxItems: 8
              }
            },
            required: [
              "instanceId",
              "nativeVersion",
              "layoutHash",
              "database",
              "originalFiles",
              "workingHash",
              "logicalDataHash",
              "changes"
            ],
            additionalProperties: false
          },
          maxItems: 128
        }
      },
      required: [
        "schemaVersion",
        "restoreId",
        "backupId",
        "manifestHash",
        "completedAt",
        "instances"
      ],
      additionalProperties: false
    },
    RestoreResult: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        restoreId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        backupId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        manifestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        configPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        state: {
          const: "disabled"
        },
        restoredBytes: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        elapsedMs: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "schemaVersion",
        "restoreId",
        "backupId",
        "manifestHash",
        "configPath",
        "state",
        "restoredBytes",
        "elapsedMs"
      ],
      additionalProperties: false
    },
    Health: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        componentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        pid: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        bootId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        startedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        ready: {
          type: "boolean"
        },
        generation: {
          anyOf: [
            {
              type: "integer",
              minimum: 1,
              maximum: 9007199254740991
            },
            {
              type: "null"
            }
          ]
        },
        launchId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        details: {
          type: "string",
          maxLength: 4096
        }
      },
      required: [
        "schemaVersion",
        "instanceId",
        "componentId",
        "buildId",
        "pid",
        "bootId",
        "startedAt",
        "observedAt",
        "ready",
        "generation",
        "details"
      ],
      additionalProperties: false
    },
    RuntimeControl: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        launchId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        bootId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        action: {
          enum: [
            "drain",
            "shutdown"
          ]
        },
        requestedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        }
      },
      required: [
        "schemaVersion",
        "instanceId",
        "launchId",
        "bootId",
        "action",
        "requestedAt"
      ],
      additionalProperties: false
    },
    ProcessStopFence: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        owner: {
          enum: [
            "runtime-owner",
            "native"
          ]
        },
        ownerId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        processId: {
          anyOf: [
            {
              type: "integer",
              minimum: 1,
              maximum: 9007199254740991
            },
            {
              type: "null"
            }
          ]
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        code: {
          const: "outcome_unknown"
        }
      },
      required: [
        "schemaVersion",
        "owner",
        "ownerId",
        "processId",
        "observedAt",
        "code"
      ],
      additionalProperties: false
    },
    RuntimeTarget: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        revision: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        candidateId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        desired: {
          enum: [
            "running",
            "stopped"
          ]
        },
        configPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        requestedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        }
      },
      required: [
        "schemaVersion",
        "instanceId",
        "revision",
        "candidateId",
        "desired",
        "configPath",
        "requestedAt"
      ],
      additionalProperties: false
    },
    RuntimeObservation: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        ownerPid: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        ownerBootId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        targetRevision: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        candidateId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        buildId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        state: {
          enum: [
            "idle",
            "starting",
            "ready",
            "draining",
            "stopped",
            "exited",
            "failed",
            "unknown"
          ]
        },
        health: {
          anyOf: [
            {
              $ref: "#/$defs/Health"
            },
            {
              type: "null"
            }
          ]
        },
        restartCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        nextRestartAt: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 64
            },
            {
              type: "null"
            }
          ]
        },
        code: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        message: {
          type: "string",
          maxLength: 4096
        }
      },
      required: [
        "schemaVersion",
        "instanceId",
        "ownerPid",
        "ownerBootId",
        "observedAt",
        "targetRevision",
        "candidateId",
        "buildId",
        "state",
        "health",
        "restartCount",
        "nextRestartAt",
        "code",
        "message"
      ],
      additionalProperties: false
    },
    ExecutorStatus: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        pid: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        bootId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        buildId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        state: {
          enum: [
            "reconciling",
            "ready",
            "stopping",
            "failed"
          ]
        },
        activeDeploymentId: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        code: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        configurationRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        configurationHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "pid",
        "bootId",
        "buildId",
        "observedAt",
        "state",
        "activeDeploymentId",
        "code"
      ],
      additionalProperties: false
    },
    LinuxResourceBudget: {
      type: "object",
      properties: {
        memoryHighBytes: {
          type: "integer",
          minimum: 67108864,
          maximum: 9007199254740991
        },
        memoryMaxBytes: {
          type: "integer",
          minimum: 134217728,
          maximum: 9007199254740991
        },
        tasksMax: {
          type: "integer",
          minimum: 16,
          maximum: 65536
        }
      },
      required: [
        "memoryHighBytes",
        "memoryMaxBytes",
        "tasksMax"
      ],
      additionalProperties: false
    },
    LinuxResourceScope: {
      type: "object",
      properties: {
        slice: {
          type: "string",
          pattern: "^ivynext[0-9a-f]{24}\\.slice$"
        },
        budget: {
          $ref: "#/$defs/LinuxResourceBudget"
        }
      },
      required: [
        "slice",
        "budget"
      ],
      additionalProperties: false
    },
    BootstrapPlan: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        os: {
          enum: [
            "win32",
            "linux"
          ]
        },
        installationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        candidateId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        artifactRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        configPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        configHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        runtimeRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        nodeExecutable: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        processes: {
          type: "array",
          items: {
            type: "object",
            properties: {
              instanceId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              componentId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              name: {
                type: "string",
                minLength: 1,
                maxLength: 64
              },
              user: {
                type: "string",
                minLength: 1,
                maxLength: 32
              },
              candidateId: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              artifactRoot: {
                type: "string",
                minLength: 1,
                maxLength: 2048
              },
              entrypoint: {
                type: "object",
                properties: {
                  executable: {
                    type: "string",
                    minLength: 1,
                    maxLength: 1024
                  },
                  args: {
                    type: "array",
                    items: {
                      type: "string",
                      minLength: 1,
                      maxLength: 4096
                    },
                    maxItems: 128
                  },
                  cwd: {
                    type: "string",
                    minLength: 1,
                    maxLength: 1024
                  },
                  timeoutMs: {
                    type: "integer",
                    minimum: 1,
                    maximum: 72e5
                  }
                },
                required: [
                  "executable",
                  "args",
                  "timeoutMs"
                ],
                additionalProperties: false
              },
              configPath: {
                type: "string",
                minLength: 1,
                maxLength: 2048
              },
              restart: {
                type: "object",
                properties: {
                  policy: {
                    enum: [
                      "never",
                      "on-failure",
                      "always"
                    ]
                  },
                  minimumDelayMs: {
                    type: "integer",
                    minimum: 100,
                    maximum: 6e5
                  },
                  maximumDelayMs: {
                    type: "integer",
                    minimum: 100,
                    maximum: 36e5
                  }
                },
                required: [
                  "policy",
                  "minimumDelayMs",
                  "maximumDelayMs"
                ],
                additionalProperties: false
              },
              allowWindowsBreakaway: {
                type: "boolean"
              }
            },
            required: [
              "instanceId",
              "componentId",
              "name"
            ],
            additionalProperties: false
          },
          maxItems: 128
        },
        linuxResources: {
          $ref: "#/$defs/LinuxResourceScope"
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "os",
        "installationId",
        "candidateId",
        "artifactRoot",
        "configPath",
        "configHash",
        "runtimeRoot",
        "nodeExecutable",
        "processes"
      ],
      additionalProperties: false
    },
    BootstrapSnapshot: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        installationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        configPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        configurationHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        owners: {
          type: "array",
          items: {
            type: "object",
            properties: {
              instanceId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              componentId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              name: {
                type: "string",
                minLength: 1,
                maxLength: 64
              },
              candidateId: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              definitionHash: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              enabled: {
                type: "boolean"
              },
              running: {
                type: "boolean"
              }
            },
            required: [
              "instanceId",
              "componentId",
              "name",
              "candidateId",
              "definitionHash",
              "enabled",
              "running"
            ],
            additionalProperties: false
          },
          maxItems: 128
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "installationId",
        "configPath",
        "configurationHash",
        "observedAt",
        "owners"
      ],
      additionalProperties: false
    },
    BootstrapMaintenanceRequest: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        operationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        previous: {
          $ref: "#/$defs/BootstrapSnapshot"
        },
        next: {
          $ref: "#/$defs/BootstrapPlan"
        }
      },
      required: [
        "schemaVersion",
        "operationId",
        "previous",
        "next"
      ],
      additionalProperties: false
    },
    BootstrapMaintenanceRecord: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        operationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        request: {
          $ref: "#/$defs/BootstrapMaintenanceRequest"
        },
        createdAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        updatedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        phase: {
          enum: [
            "checking",
            "updating",
            "succeeded",
            "needs_attention"
          ]
        },
        errorCode: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        definitions: {
          type: "array",
          items: {
            type: "object",
            properties: {
              instanceId: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              name: {
                type: "string",
                minLength: 1,
                maxLength: 64
              },
              previousHash: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              desiredHash: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              applied: {
                type: "boolean"
              }
            },
            required: [
              "instanceId",
              "name",
              "previousHash",
              "desiredHash",
              "applied"
            ],
            additionalProperties: false
          },
          maxItems: 128
        }
      },
      required: [
        "schemaVersion",
        "operationId",
        "requestHash",
        "request",
        "createdAt",
        "updatedAt",
        "phase",
        "errorCode",
        "definitions"
      ],
      additionalProperties: false
    },
    ContainerState: {
      type: "object",
      properties: {
        containerId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        name: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        imageId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        candidateId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        launchId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        state: {
          enum: [
            "created",
            "running",
            "paused",
            "restarting",
            "exited",
            "dead",
            "removing"
          ]
        },
        exitCode: {
          type: "integer",
          minimum: 0,
          maximum: 255
        },
        startedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        cgroupParent: {
          type: "string",
          maxLength: 2048
        }
      },
      required: [
        "containerId",
        "name",
        "imageId",
        "hostId",
        "instanceId",
        "candidateId",
        "launchId",
        "state",
        "exitCode",
        "startedAt"
      ],
      additionalProperties: false
    },
    StorageInspection: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        exists: {
          type: "boolean"
        },
        format: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        contracts: {
          type: "array",
          items: {
            type: "object",
            properties: {
              key: {
                type: "string",
                minLength: 1,
                maxLength: 192
              },
              versions: {
                type: "array",
                items: {
                  type: "string",
                  minLength: 1,
                  maxLength: 128
                },
                maxItems: 64
              }
            },
            required: [
              "key",
              "versions"
            ],
            additionalProperties: false
          },
          maxItems: 128
        }
      },
      required: [
        "schemaVersion",
        "exists",
        "format",
        "contracts"
      ],
      additionalProperties: false
    },
    ManagerSettings: {
      type: "object",
      properties: {
        hostConfigPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        }
      },
      required: [
        "hostConfigPath"
      ],
      additionalProperties: false
    },
    ManagedInstance: {
      type: "object",
      properties: {
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        componentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        engine: {
          enum: [
            "process",
            "docker"
          ]
        },
        desiredEnabled: {
          type: "boolean"
        },
        installedBuild: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        installedCandidateId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        observedBuild: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        observedState: {
          anyOf: [
            {
              enum: [
                "idle",
                "starting",
                "ready",
                "draining",
                "stopped",
                "exited",
                "failed",
                "unknown"
              ]
            },
            {
              type: "null"
            }
          ]
        },
        observedAt: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 64
            },
            {
              type: "null"
            }
          ]
        },
        code: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        message: {
          type: "string",
          maxLength: 512
        }
      },
      required: [
        "instanceId",
        "serviceNodeId",
        "componentId",
        "engine",
        "desiredEnabled",
        "installedBuild",
        "installedCandidateId",
        "observedBuild",
        "observedState",
        "observedAt",
        "code",
        "message"
      ],
      additionalProperties: false
    },
    HostStatus: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        executor: {
          anyOf: [
            {
              $ref: "#/$defs/ExecutorStatus"
            },
            {
              type: "null"
            }
          ]
        },
        instances: {
          type: "array",
          items: {
            $ref: "#/$defs/ManagedInstance"
          },
          maxItems: 128
        },
        unfinished: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DeploymentRecord"
          },
          maxItems: 200
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "observedAt",
        "executor",
        "instances",
        "unfinished"
      ],
      additionalProperties: false
    },
    ManagementSnapshot: {
      type: "object",
      properties: {
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        status: {
          $ref: "#/$defs/HostStatus"
        }
      },
      required: [
        "sequence",
        "status"
      ],
      additionalProperties: false
    },
    ManagementObservation: {
      type: "object",
      properties: {
        snapshot: {
          $ref: "#/$defs/ManagementSnapshot"
        },
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        reportedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        stale: {
          type: "boolean"
        }
      },
      required: [
        "snapshot",
        "serviceNodeId",
        "reportedAt",
        "stale"
      ],
      additionalProperties: false
    },
    ManagerStatusInput: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    ManagerDeploymentInput: {
      type: "object",
      properties: {
        deploymentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        }
      },
      required: [
        "deploymentId"
      ],
      additionalProperties: false
    },
    ManagerDeployInput: {
      type: "object",
      properties: {
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        operationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        candidateId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        source: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        }
      },
      required: [
        "instanceId",
        "operationId"
      ],
      additionalProperties: false
    },
    ManagerRollbackInput: {
      type: "object",
      properties: {
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        operationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        targetBuild: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "instanceId",
        "operationId",
        "targetBuild"
      ],
      additionalProperties: false
    },
    ManagerLifecycleInput: {
      type: "object",
      properties: {
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        operationId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        }
      },
      required: [
        "instanceId",
        "operationId"
      ],
      additionalProperties: false
    },
    CliOutput: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        ok: {
          type: "boolean"
        },
        code: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        deploymentId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        data: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        }
      },
      required: [
        "schemaVersion",
        "ok",
        "code",
        "data"
      ],
      additionalProperties: false
    },
    HostConfigInput: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        developmentMode: {
          type: "boolean"
        },
        ivyRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        servicesRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        configPath: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        runtimeRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        artifactRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        stagingRoot: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        publicBaseUrl: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        executables: {
          type: "object",
          minProperties: 1,
          maxProperties: 32,
          additionalProperties: {
            type: "string",
            minLength: 1,
            maxLength: 2048
          },
          properties: {
            dotnet: {
              type: "string",
              minLength: 1,
              maxLength: 2048,
              description: "Optional absolute .NET SDK executable for nested preparation/check commands; forwarded as IVY_DOTNET."
            },
            gcc: {
              type: "string",
              minLength: 1,
              maxLength: 2048,
              description: "Optional absolute MinGW C compiler for nested preparation/check commands; forwarded as IVY_EVS_CC."
            }
          }
        },
        instances: {
          type: "array",
          items: {
            $ref: "#/$defs/Instance"
          },
          maxItems: 128
        },
        deployment: {
          type: "object",
          properties: {
            identity: {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            hosts: {
              type: "array",
              items: {
                type: "object",
                properties: {
                  hostId: {
                    type: "string",
                    minLength: 1,
                    maxLength: 128
                  },
                  configPath: {
                    type: "string",
                    minLength: 1,
                    maxLength: 2048
                  },
                  ssh: {
                    type: "string",
                    minLength: 1,
                    maxLength: 256
                  },
                  cliPath: {
                    type: "string",
                    minLength: 1,
                    maxLength: 2048
                  }
                },
                required: [
                  "hostId",
                  "configPath"
                ],
                additionalProperties: false
              },
              maxItems: 32
            }
          },
          required: [
            "identity",
            "hosts"
          ],
          additionalProperties: false
        },
        configurationUpdates: {
          type: "object",
          properties: {
            intervalSeconds: {
              type: "integer",
              minimum: 2,
              maximum: 3600
            }
          },
          required: [
            "intervalSeconds"
          ],
          additionalProperties: false
        },
        packageUpdates: {
          type: "object",
          properties: {
            intervalSeconds: {
              type: "integer",
              minimum: 2,
              maximum: 3600
            }
          },
          required: [
            "intervalSeconds"
          ],
          additionalProperties: false
        },
        restoredFrom: {
          type: "object",
          properties: {
            backupId: {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            manifestHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            restoreId: {
              type: "string",
              minLength: 1,
              maxLength: 128
            }
          },
          required: [
            "backupId",
            "manifestHash",
            "restoreId"
          ],
          additionalProperties: false
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "publicBaseUrl",
        "executables",
        "instances"
      ],
      additionalProperties: false
    }
  }
};

// packages/contracts/src/host-validation.ts
import { Ajv2020 as Ajv20202 } from "ajv/dist/2020.js";
var schema = host_schema_default;
var ajv = new Ajv20202({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
ajv.addSchema(schema);
ajv.addSchema(hive_wire_schema_default);
function validateHost(name, value) {
  canonical(value, 32 * 1024 * 1024);
  const validator = ajv.getSchema(`${schema.$id}#/$defs/${name}`);
  requireThat(validator && validator(value), "invalid_arguments", "Value does not match its host configuration/operation contract.");
}
var hostSchema = schema;

export {
  pointerParts,
  admitSchema,
  SchemaValidators,
  host_schema_default,
  validateHost,
  hostSchema
};
//# sourceMappingURL=chunk-MEXDZTG7.js.map
