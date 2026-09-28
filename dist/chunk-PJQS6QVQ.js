import {
  IvyError,
  canonical,
  connectionInFlightRequests,
  encodeJson,
  managementFrameBytes,
  requireThat
} from "./chunk-T2KPXKB3.js";

// packages/sdk/src/request-queue.ts
var RequestQueue = class {
  constructor(maximum, providerMaximum, waitingMaximum = 128) {
    this.maximum = maximum;
    this.providerMaximum = providerMaximum;
    this.waitingMaximum = waitingMaximum;
    requireThat(
      [maximum, providerMaximum, waitingMaximum].every(
        (value) => Number.isSafeInteger(value) && value > 0
      ) && providerMaximum <= maximum,
      "invalid_arguments",
      "Invalid client request admission limits."
    );
  }
  maximum;
  providerMaximum;
  waitingMaximum;
  active = 0;
  providers = 0;
  waiting = [];
  acquire(provider, signal) {
    signal.throwIfAborted();
    const available = () => this.active < this.maximum && (!provider || this.providers < this.providerMaximum);
    if (available()) return Promise.resolve(this.enter(provider));
    if (this.waiting.length >= this.waitingMaximum)
      return Promise.reject(
        new IvyError(
          "limit_exceeded",
          "Hive client request queue is full.",
          "not_executed",
          {
            budget: "client_queue",
            active: this.waiting.length,
            limit: this.waitingMaximum
          }
        )
      );
    return new Promise((resolve, reject) => {
      const abort = () => {
        const index = this.waiting.indexOf(entry);
        if (index >= 0) this.waiting.splice(index, 1);
        reject(signal.reason);
      };
      const entry = {
        provider,
        signal,
        start: () => {
          signal.removeEventListener("abort", abort);
          resolve(this.enter(provider));
        }
      };
      signal.addEventListener("abort", abort, { once: true });
      this.waiting.push(entry);
    });
  }
  enter(provider) {
    this.active++;
    if (provider) this.providers++;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.active--;
      if (provider) this.providers--;
      while (this.active < this.maximum) {
        const index = this.waiting.findIndex(
          (entry) => !entry.provider || this.providers < this.providerMaximum
        );
        if (index < 0) break;
        this.waiting.splice(index, 1)[0].start();
      }
    };
  }
};

// packages/contracts/src/operation-id.ts
function operationId(runtimeEpoch2, issuedAtUnixMs = Date.now(), nonce = crypto.randomUUID()) {
  requireThat(runtimeEpoch2.length > 0 && !runtimeEpoch2.includes(":") && Number.isSafeInteger(issuedAtUnixMs) && issuedAtUnixMs >= 0 && nonce.length > 0 && nonce.length <= 128 && !nonce.includes(":"), "invalid_arguments", "Invalid operation identity fields.");
  return `${runtimeEpoch2}:${issuedAtUnixMs}:${nonce}`;
}
function parseOperationId(value) {
  const parts = value.split(":");
  requireThat(
    parts.length === 3 && parts[0] && /^\d{1,16}$/.test(parts[1] ?? "") && parts[2] && parts[2].length <= 128,
    "invalid_arguments",
    "Operation identity must be <runtimeEpoch>:<issuedAtUnixMs>:<nonce>."
  );
  const issuedAtUnixMs = Number(parts[1]);
  requireThat(Number.isSafeInteger(issuedAtUnixMs), "invalid_arguments", "Operation identity has an invalid issue time.");
  return { runtimeEpoch: parts[0], issuedAtUnixMs, nonce: parts[2] };
}
function deriveOperationId(parent, scope) {
  const identity = parseOperationId(parent);
  const text2 = canonical({ nonce: identity.nonce, scope });
  let first = 2166136261, second = 2246822519;
  for (let index = 0; index < text2.length; index++) {
    const code = text2.charCodeAt(index);
    first = Math.imul(first ^ code, 16777619);
    second = Math.imul(second ^ code, 3266489917);
  }
  return operationId(
    identity.runtimeEpoch,
    identity.issuedAtUnixMs,
    (first >>> 0).toString(16).padStart(8, "0") + (second >>> 0).toString(16).padStart(8, "0")
  );
}

// specs/schemas/agent.schema.json
var agent_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/agent.schema.json",
  title: "Native Codex projection and AgentManager ownership, observations and outcome journal",
  $defs: {
    Catalog: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        provider: {
          const: "codex"
        },
        version: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        experimental: {
          const: true
        },
        sourceHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        clientRequests: {
          type: "array",
          items: {
            type: "object",
            properties: {
              method: {
                type: "string",
                maxLength: 192,
                minLength: 1
              },
              paramsRequired: {
                type: "boolean"
              },
              inputSchema: {
                anyOf: [
                  {
                    type: "boolean"
                  },
                  {
                    type: "object",
                    additionalProperties: {
                      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                    }
                  }
                ]
              },
              outputSchema: {
                anyOf: [
                  {
                    type: "boolean"
                  },
                  {
                    type: "object",
                    additionalProperties: {
                      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                    }
                  }
                ]
              },
              responseType: {
                type: "string",
                maxLength: 256,
                minLength: 1
              },
              inputSource: {
                enum: [
                  "native-json",
                  "native-typescript-and-rust"
                ]
              },
              outputSource: {
                enum: [
                  "native-json",
                  "native-typescript-and-rust"
                ]
              }
            },
            required: [
              "method",
              "paramsRequired",
              "inputSchema",
              "outputSchema",
              "responseType",
              "inputSource",
              "outputSource"
            ],
            additionalProperties: false
          },
          maxItems: 256
        },
        serverRequests: {
          type: "array",
          items: {
            type: "object",
            properties: {
              method: {
                type: "string",
                maxLength: 192,
                minLength: 1
              },
              paramsRequired: {
                type: "boolean"
              },
              inputSchema: {
                anyOf: [
                  {
                    type: "boolean"
                  },
                  {
                    type: "object",
                    additionalProperties: {
                      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                    }
                  }
                ]
              },
              outputSchema: {
                anyOf: [
                  {
                    type: "boolean"
                  },
                  {
                    type: "object",
                    additionalProperties: {
                      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                    }
                  }
                ]
              },
              responseType: {
                type: "string",
                maxLength: 256,
                minLength: 1
              },
              inputSource: {
                enum: [
                  "native-json",
                  "native-typescript-and-rust"
                ]
              },
              outputSource: {
                enum: [
                  "native-json",
                  "native-typescript-and-rust"
                ]
              }
            },
            required: [
              "method",
              "paramsRequired",
              "inputSchema",
              "outputSchema",
              "responseType",
              "inputSource",
              "outputSource"
            ],
            additionalProperties: false
          },
          maxItems: 64
        },
        serverNotifications: {
          type: "array",
          items: {
            type: "object",
            properties: {
              method: {
                type: "string",
                maxLength: 192,
                minLength: 1
              },
              paramsRequired: {
                type: "boolean"
              },
              inputSchema: {
                anyOf: [
                  {
                    type: "boolean"
                  },
                  {
                    type: "object",
                    additionalProperties: {
                      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                    }
                  }
                ]
              },
              paramsAbsent: {
                type: "boolean"
              },
              inputSource: {
                enum: [
                  "native-json",
                  "native-typescript-and-json-types"
                ]
              }
            },
            required: [
              "method",
              "paramsRequired",
              "inputSchema",
              "paramsAbsent",
              "inputSource"
            ],
            additionalProperties: false
          },
          maxItems: 256
        },
        clientNotifications: {
          type: "array",
          items: {
            type: "object",
            properties: {
              method: {
                type: "string",
                maxLength: 192,
                minLength: 1
              },
              paramsRequired: {
                type: "boolean"
              },
              inputSchema: {
                anyOf: [
                  {
                    type: "boolean"
                  },
                  {
                    type: "object",
                    additionalProperties: {
                      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                    }
                  }
                ]
              },
              paramsAbsent: {
                type: "boolean"
              },
              inputSource: {
                enum: [
                  "native-json",
                  "native-typescript-and-json-types"
                ]
              }
            },
            required: [
              "method",
              "paramsRequired",
              "inputSchema",
              "paramsAbsent",
              "inputSource"
            ],
            additionalProperties: false
          },
          maxItems: 64
        },
        derivedLegacyTypes: {
          type: "array",
          items: {
            type: "string",
            maxLength: 256,
            minLength: 1
          },
          maxItems: 64
        }
      },
      required: [
        "schemaVersion",
        "provider",
        "version",
        "experimental",
        "sourceHash",
        "nativeExecutableHash",
        "clientRequests",
        "serverRequests",
        "serverNotifications",
        "clientNotifications",
        "derivedLegacyTypes"
      ],
      additionalProperties: false
    },
    ParametersRef: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        catalogHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        method: {
          type: "string",
          maxLength: 192,
          minLength: 1
        },
        reservedFields: {
          type: "array",
          items: {
            type: "string",
            maxLength: 256,
            minLength: 1
          },
          maxItems: 64
        },
        params: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        }
      },
      required: [
        "schemaVersion",
        "nativeVersion",
        "nativeExecutableHash",
        "catalogHash",
        "method",
        "reservedFields",
        "params"
      ],
      additionalProperties: false
    },
    PlanDefinitions: {
      type: "object",
      properties: {
        threadStart: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        threadResume: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        turnStart: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        turnInterrupt: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "threadStart",
        "threadResume",
        "turnStart",
        "turnInterrupt"
      ],
      additionalProperties: false
    },
    PlanLocation: {
      type: "object",
      properties: {
        serviceNodeId: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        hostId: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        kind: {
          enum: [
            "existing",
            "normal",
            "internal"
          ]
        },
        cwd: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        projectId: {
          type: "string",
          maxLength: 256,
          minLength: 1
        }
      },
      required: [
        "serviceNodeId",
        "hostId",
        "kind",
        "cwd",
        "projectId"
      ],
      additionalProperties: false
    },
    Plan: {
      type: "object",
      properties: {
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        catalogSourceHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        definitions: {
          $ref: "#/$defs/PlanDefinitions"
        },
        location: {
          $ref: "#/$defs/PlanLocation"
        },
        threadStart: {
          $ref: "#/$defs/ParametersRef"
        },
        threadResume: {
          $ref: "#/$defs/ParametersRef"
        },
        turnStart: {
          $ref: "#/$defs/ParametersRef"
        }
      },
      required: [
        "nativeVersion",
        "catalogSourceHash",
        "definitions",
        "threadStart",
        "threadResume",
        "turnStart"
      ],
      additionalProperties: false
    },
    PlanDraft: {
      type: "object",
      properties: {
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        catalogSourceHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        definitions: {
          $ref: "#/$defs/PlanDefinitions"
        },
        location: {
          $ref: "#/$defs/PlanLocation"
        },
        threadStart: {
          type: "object",
          additionalProperties: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
          }
        },
        threadResume: {
          type: "object",
          additionalProperties: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
          }
        },
        turnStart: {
          type: "object",
          additionalProperties: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
          }
        }
      },
      required: [
        "nativeVersion",
        "catalogSourceHash",
        "definitions",
        "threadStart",
        "threadResume",
        "turnStart"
      ],
      additionalProperties: false
    },
    InvocationDraft: {
      type: "object",
      properties: {
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        catalogSourceHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        method: {
          enum: [
            "thread/start",
            "thread/resume",
            "turn/start",
            "turn/interrupt"
          ]
        },
        params: {
          type: "object",
          additionalProperties: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
          }
        }
      },
      required: [
        "nativeVersion",
        "catalogSourceHash",
        "method",
        "params"
      ],
      additionalProperties: false
    },
    WindowsShell: {
      type: "object",
      properties: {
        executable: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        executableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "executable",
        "executableHash"
      ],
      additionalProperties: false
    },
    WindowsShellStatus: {
      type: "object",
      properties: {
        executable: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        executableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        version: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        packageIdentity: {
          const: "unpackaged"
        }
      },
      required: [
        "executable",
        "executableHash",
        "version",
        "packageIdentity"
      ],
      additionalProperties: false
    },
    InstructionsDocument: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        enabled: {
          anyOf: [
            {
              type: "boolean"
            },
            {
              type: "null"
            }
          ]
        },
        includeLocal: {
          type: "boolean"
        },
        text: {
          type: "string",
          maxLength: 16384
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "enabled",
        "includeLocal",
        "text"
      ],
      additionalProperties: false
    },
    InstructionsStatus: {
      type: "object",
      properties: {
        state: {
          enum: [
            "disabled",
            "pending",
            "applied",
            "conflict"
          ]
        },
        home: {
          anyOf: [
            {
              type: "string",
              maxLength: 2048
            },
            {
              type: "null"
            }
          ]
        },
        desiredVersion: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        appliedVersion: {
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
          maxLength: 64,
          minLength: 1
        },
        code: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        bytes: {
          type: "integer",
          minimum: 0,
          maximum: 32768
        }
      },
      required: [
        "state",
        "home",
        "desiredVersion",
        "appliedVersion",
        "observedAt",
        "code",
        "bytes"
      ],
      additionalProperties: false
    },
    ManagedOutputStatus: {
      type: "object",
      properties: {
        state: {
          enum: [
            "disabled",
            "pending",
            "applied",
            "conflict"
          ]
        },
        target: {
          anyOf: [
            {
              type: "string",
              maxLength: 2048
            },
            {
              type: "null"
            }
          ]
        },
        desiredVersion: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            {
              type: "null"
            }
          ]
        },
        appliedVersion: {
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
          maxLength: 64,
          minLength: 1
        },
        code: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        bytes: {
          type: "integer",
          minimum: 0,
          maximum: 4194304
        }
      },
      required: [
        "state",
        "target",
        "desiredVersion",
        "appliedVersion",
        "observedAt",
        "code",
        "bytes"
      ],
      additionalProperties: false
    },
    McpServer: {
      type: "object",
      properties: {
        name: {
          type: "string",
          maxLength: 64,
          minLength: 1,
          pattern: "^[a-z][a-z0-9_-]{0,63}$"
        },
        url: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        enabled: {
          type: "boolean"
        },
        authentication: {
          enum: [
            "none",
            "agent-manager"
          ]
        },
        startupTimeoutSeconds: {
          type: "integer",
          minimum: 1,
          maximum: 300
        },
        toolTimeoutSeconds: {
          type: "integer",
          minimum: 1,
          maximum: 3600
        }
      },
      required: [
        "name",
        "url",
        "enabled",
        "authentication",
        "startupTimeoutSeconds",
        "toolTimeoutSeconds"
      ],
      additionalProperties: false
    },
    McpDocument: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        enabled: {
          anyOf: [
            {
              type: "boolean"
            },
            {
              type: "null"
            }
          ]
        },
        servers: {
          type: "array",
          items: {
            $ref: "#/$defs/McpServer"
          },
          maxItems: 32
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "enabled",
        "servers"
      ],
      additionalProperties: false
    },
    SkillFile: {
      type: "object",
      properties: {
        path: {
          type: "string",
          maxLength: 512,
          minLength: 1
        },
        content: {
          type: "string",
          maxLength: 262144
        }
      },
      required: [
        "path",
        "content"
      ],
      additionalProperties: false
    },
    SkillsDocument: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        hostId: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        enabled: {
          anyOf: [
            {
              type: "boolean"
            },
            {
              type: "null"
            }
          ]
        },
        files: {
          type: "array",
          items: {
            $ref: "#/$defs/SkillFile"
          },
          maxItems: 256
        }
      },
      required: [
        "schemaVersion",
        "hostId",
        "enabled",
        "files"
      ],
      additionalProperties: false
    },
    EnvironmentDefaults: {
      type: "object",
      properties: {
        instructions: {
          $ref: "#/$defs/InstructionsDocument"
        },
        mcp: {
          $ref: "#/$defs/McpDocument"
        },
        skills: {
          $ref: "#/$defs/SkillsDocument"
        }
      },
      required: [
        "instructions",
        "mcp",
        "skills"
      ],
      additionalProperties: false
    },
    EnvironmentDefaultsInput: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    EnvironmentStatus: {
      type: "object",
      properties: {
        mcp: {
          $ref: "#/$defs/ManagedOutputStatus"
        },
        skills: {
          $ref: "#/$defs/ManagedOutputStatus"
        }
      },
      required: [
        "mcp",
        "skills"
      ],
      additionalProperties: false
    },
    ExecutionCapability: {
      type: "object",
      properties: {
        key: {
          type: "string",
          maxLength: 64,
          minLength: 1,
          pattern: "^[a-z0-9]+(?:-[a-z0-9]+)*$"
        },
        label: {
          type: "string",
          maxLength: 128,
          minLength: 1
        }
      },
      required: [
        "key",
        "label"
      ],
      additionalProperties: false
    },
    CapabilityProfile: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        capabilities: {
          type: "array",
          items: {
            $ref: "#/$defs/ExecutionCapability"
          },
          maxItems: 64
        },
        updatedAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        },
        operationId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "serviceNodeId",
        "hostId",
        "revision",
        "capabilities",
        "updatedAt",
        "operationId"
      ],
      additionalProperties: false
    },
    CapabilityProfileInput: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    ConfigureCapabilitiesInput: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        capabilities: {
          type: "array",
          items: {
            $ref: "#/$defs/ExecutionCapability"
          },
          maxItems: 64
        }
      },
      required: [
        "operationId",
        "expectedRevision",
        "capabilities"
      ],
      additionalProperties: false
    },
    Settings: {
      type: "object",
      properties: {
        nativeExecutable: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        nativeHome: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        codexHome: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        skillsRoot: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        patcherEnabled: {
          type: "boolean"
        },
        projectRoot: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        internalProjectRoot: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        appServer: {
          oneOf: [
            {
              type: "object",
              properties: {
                mode: {
                  const: "owned-stdio"
                }
              },
              required: [
                "mode"
              ],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                mode: {
                  const: "external-proxy"
                },
                socketPath: {
                  type: "string",
                  maxLength: 2048,
                  minLength: 1
                },
                expectedCodexHome: {
                  type: "string",
                  maxLength: 2048,
                  minLength: 1
                }
              },
              required: [
                "mode"
              ],
              additionalProperties: false
            }
          ]
        },
        windowsShell: {
          $ref: "#/$defs/WindowsShell"
        },
        capabilities: {
          type: "array",
          items: {
            $ref: "#/$defs/ExecutionCapability"
          },
          maxItems: 64
        },
        projects: {
          type: "array",
          items: {
            type: "object",
            properties: {
              id: {
                type: "string",
                maxLength: 256,
                minLength: 1
              },
              name: {
                type: "string",
                maxLength: 256,
                minLength: 1
              },
              path: {
                type: "string",
                maxLength: 2048,
                minLength: 1
              }
            },
            required: [
              "id",
              "name",
              "path"
            ],
            additionalProperties: false
          },
          maxItems: 256
        },
        limits: {
          type: "object",
          properties: {
            maxOperations: {
              type: "integer",
              minimum: 100,
              maximum: 1e6
            },
            maxJournalBytes: {
              type: "integer",
              minimum: 1048576,
              maximum: 17179869184
            },
            maxPendingInputs: {
              type: "integer",
              minimum: 1,
              maximum: 128
            },
            maxNotificationBytes: {
              type: "integer",
              minimum: 1048576,
              maximum: 268435456
            }
          },
          required: [
            "maxOperations",
            "maxJournalBytes",
            "maxPendingInputs",
            "maxNotificationBytes"
          ],
          additionalProperties: false
        }
      },
      required: [
        "nativeExecutable",
        "nativeVersion",
        "nativeExecutableHash",
        "projects",
        "limits"
      ],
      additionalProperties: false
    },
    RequestId: {
      anyOf: [
        {
          type: "string"
        },
        {
          type: "integer",
          minimum: -9007199254740991,
          maximum: 9007199254740991
        }
      ]
    },
    NativeError: {
      type: "object",
      properties: {
        code: {
          type: "integer"
        },
        message: {
          type: "string",
          maxLength: 65536
        },
        data: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        }
      },
      required: [
        "code",
        "message"
      ],
      additionalProperties: false
    },
    Reply: {
      oneOf: [
        {
          type: "object",
          properties: {
            result: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
            }
          },
          required: [
            "result"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            error: {
              $ref: "#/$defs/NativeError"
            }
          },
          required: [
            "error"
          ],
          additionalProperties: false
        }
      ]
    },
    Operation: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        method: {
          type: "string",
          maxLength: 192,
          minLength: 1
        },
        params: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        requestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        phase: {
          enum: [
            "accepted",
            "dispatched",
            "succeeded",
            "failed",
            "outcome_unknown"
          ]
        },
        createdAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        },
        updatedAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        },
        epoch: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        requestId: {
          anyOf: [
            {
              $ref: "#/$defs/RequestId"
            },
            {
              type: "null"
            }
          ]
        },
        reply: {
          anyOf: [
            {
              $ref: "#/$defs/Reply"
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
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "schemaVersion",
        "operationId",
        "callerPrincipalId",
        "serviceNodeId",
        "nativeVersion",
        "nativeExecutableHash",
        "method",
        "params",
        "requestHash",
        "phase",
        "createdAt",
        "updatedAt",
        "epoch",
        "requestId",
        "reply",
        "code"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              phase: {
                enum: [
                  "accepted",
                  "dispatched",
                  "outcome_unknown"
                ]
              }
            }
          },
          then: {
            properties: {
              reply: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "succeeded"
              }
            }
          },
          then: {
            properties: {
              reply: {
                type: "object",
                properties: {
                  result: {
                    $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                  }
                },
                required: [
                  "result"
                ],
                additionalProperties: false
              },
              code: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "failed"
              }
            }
          },
          then: {
            properties: {
              reply: {
                anyOf: [
                  {
                    type: "object",
                    properties: {
                      error: {
                        $ref: "#/$defs/NativeError"
                      }
                    },
                    required: [
                      "error"
                    ],
                    additionalProperties: false
                  },
                  {
                    type: "null"
                  }
                ]
              },
              code: {
                type: "string",
                maxLength: 256,
                minLength: 1
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "outcome_unknown"
              }
            }
          },
          then: {
            properties: {
              code: {
                type: "string",
                maxLength: 256,
                minLength: 1
              },
              epoch: {
                type: "string",
                maxLength: 256,
                minLength: 1
              },
              requestId: {
                $ref: "#/$defs/RequestId"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                const: "accepted"
              }
            }
          },
          then: {
            properties: {
              epoch: {
                type: "null"
              },
              requestId: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              phase: {
                enum: [
                  "dispatched",
                  "succeeded"
                ]
              }
            }
          },
          then: {
            properties: {
              epoch: {
                type: "string",
                maxLength: 256,
                minLength: 1
              },
              requestId: {
                $ref: "#/$defs/RequestId"
              }
            }
          }
        }
      ]
    },
    InputIdentity: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        requestId: {
          $ref: "#/$defs/RequestId"
        }
      },
      required: [
        "serviceNodeId",
        "epoch",
        "requestId"
      ],
      additionalProperties: false
    },
    InputNotification: {
      type: "object",
      properties: {
        identity: {
          $ref: "#/$defs/InputIdentity"
        },
        threadId: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "identity",
        "threadId"
      ],
      additionalProperties: false
    },
    PendingInput: {
      type: "object",
      properties: {
        identity: {
          $ref: "#/$defs/InputIdentity"
        },
        method: {
          type: "string",
          maxLength: 192,
          minLength: 1
        },
        params: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        observedAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        },
        updatedAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        },
        threadId: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        turnId: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        state: {
          enum: [
            "pending",
            "answering",
            "answered",
            "expired",
            "outcome_unknown"
          ]
        },
        answerOperationId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        answerCallerPrincipalId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        reply: {
          anyOf: [
            {
              $ref: "#/$defs/Reply"
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
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "identity",
        "method",
        "params",
        "observedAt",
        "updatedAt",
        "threadId",
        "turnId",
        "state",
        "answerOperationId",
        "answerCallerPrincipalId",
        "reply",
        "code"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              state: {
                const: "pending"
              }
            }
          },
          then: {
            properties: {
              answerOperationId: {
                type: "null"
              },
              answerCallerPrincipalId: {
                type: "null"
              },
              reply: {
                type: "null"
              },
              code: {
                type: "null"
              }
            }
          }
        },
        {
          if: {
            properties: {
              state: {
                enum: [
                  "answering",
                  "answered"
                ]
              }
            }
          },
          then: {
            properties: {
              answerOperationId: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              },
              answerCallerPrincipalId: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
              },
              reply: {
                $ref: "#/$defs/Reply"
              }
            }
          }
        }
      ]
    },
    Notification: {
      type: "object",
      properties: {
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        method: {
          type: "string",
          maxLength: 192,
          minLength: 1
        },
        params: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        observedAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        }
      },
      required: [
        "sequence",
        "serviceNodeId",
        "epoch",
        "nativeVersion",
        "method",
        "params",
        "observedAt"
      ],
      additionalProperties: false
    },
    Status: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        catalogHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        epoch: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        processStops: {
          type: "array",
          items: {
            type: "object",
            properties: {
              epoch: {
                type: "string",
                maxLength: 256,
                minLength: 1
              },
              observedAt: {
                type: "string",
                maxLength: 64,
                minLength: 1
              }
            },
            required: [
              "epoch",
              "observedAt"
            ],
            additionalProperties: false
          },
          maxItems: 256
        },
        pid: {
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
        state: {
          enum: [
            "starting",
            "initializing",
            "ready",
            "stopping",
            "stopped",
            "failed"
          ]
        },
        observedAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        },
        code: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        initialized: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
            },
            {
              type: "null"
            }
          ]
        },
        pendingInputs: {
          type: "integer",
          minimum: 0,
          maximum: 128
        },
        operations: {
          type: "object",
          properties: {
            retained: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            maximum: {
              type: "integer",
              minimum: 100,
              maximum: 1e6
            },
            bytes: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            maximumBytes: {
              type: "integer",
              minimum: 1048576,
              maximum: 17179869184
            }
          },
          required: [
            "retained",
            "maximum",
            "bytes",
            "maximumBytes"
          ],
          additionalProperties: false
        },
        observedMethods: {
          type: "array",
          items: {
            type: "object",
            properties: {
              method: {
                type: "string",
                maxLength: 192,
                minLength: 1
              },
              lastSucceededAt: {
                anyOf: [
                  {
                    type: "string",
                    maxLength: 64,
                    minLength: 1
                  },
                  {
                    type: "null"
                  }
                ]
              },
              lastFailedAt: {
                anyOf: [
                  {
                    type: "string",
                    maxLength: 64,
                    minLength: 1
                  },
                  {
                    type: "null"
                  }
                ]
              },
              lastCode: {
                anyOf: [
                  {
                    type: "string",
                    maxLength: 256,
                    minLength: 1
                  },
                  {
                    type: "null"
                  }
                ]
              }
            },
            required: [
              "method",
              "lastSucceededAt",
              "lastFailedAt",
              "lastCode"
            ],
            additionalProperties: false
          },
          maxItems: 256
        },
        windowsShell: {
          anyOf: [
            {
              $ref: "#/$defs/WindowsShellStatus"
            },
            {
              type: "null"
            }
          ]
        },
        instructions: {
          $ref: "#/$defs/InstructionsStatus"
        },
        environment: {
          $ref: "#/$defs/EnvironmentStatus"
        },
        capabilities: {
          $ref: "#/$defs/CapabilityProfile"
        },
        connection: {
          type: "object",
          properties: {
            mode: {
              enum: [
                "owned-stdio",
                "external-proxy"
              ]
            },
            ownsServer: {
              type: "boolean"
            },
            ownsHome: {
              type: "boolean"
            },
            actualHome: {
              type: "string",
              maxLength: 2048
            },
            actualVersion: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            socketPath: {
              anyOf: [
                {
                  type: "string",
                  maxLength: 2048
                },
                {
                  type: "null"
                }
              ]
            },
            serverPid: {
              anyOf: [
                {
                  type: "integer",
                  minimum: 1,
                  maximum: 2147483647
                },
                {
                  type: "null"
                }
              ]
            },
            lifecycle: {
              enum: [
                "started",
                "alreadyRunning",
                "explicit-endpoint",
                "owned-stdio"
              ]
            },
            mcpIdentity: {
              enum: [
                "shared-user-home",
                "instance-environment"
              ]
            }
          },
          required: [
            "mode",
            "ownsServer",
            "ownsHome",
            "actualHome",
            "actualVersion",
            "socketPath",
            "serverPid",
            "lifecycle",
            "mcpIdentity"
          ],
          additionalProperties: false
        }
      },
      required: [
        "serviceNodeId",
        "hostId",
        "nativeVersion",
        "nativeExecutableHash",
        "catalogHash",
        "epoch",
        "pid",
        "state",
        "observedAt",
        "code",
        "initialized",
        "pendingInputs",
        "operations",
        "observedMethods"
      ],
      additionalProperties: false
    },
    ThreadSummary: {
      type: "object",
      properties: {
        nativeId: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        owner: {
          enum: [
            "this-native-connection",
            "historical-unattached"
          ]
        },
        epoch: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        preview: {
          type: "string",
          maxLength: 2048
        },
        cwd: {
          anyOf: [
            {
              type: "string",
              maxLength: 2048
            },
            {
              type: "null"
            }
          ]
        },
        name: {
          anyOf: [
            {
              type: "string",
              maxLength: 512
            },
            {
              type: "null"
            }
          ]
        },
        status: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        source: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        updatedAt: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        projectId: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        recencyAt: {
          anyOf: [
            {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            {
              type: "null"
            }
          ]
        },
        canAcceptDirectInput: {
          anyOf: [
            {
              type: "boolean"
            },
            {
              type: "null"
            }
          ]
        },
        threadSource: {
          anyOf: [
            {
              type: "string",
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        archived: {
          type: "boolean"
        },
        ephemeral: {
          type: "boolean"
        }
      },
      required: [
        "nativeId",
        "owner",
        "epoch",
        "preview",
        "cwd",
        "name",
        "status",
        "source",
        "updatedAt",
        "projectId",
        "recencyAt",
        "canAcceptDirectInput",
        "threadSource",
        "archived",
        "ephemeral"
      ],
      additionalProperties: false
    },
    ProjectSummary: {
      type: "object",
      properties: {
        nativeId: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        source: {
          enum: [
            "native",
            "host-configuration"
          ]
        },
        name: {
          type: "string",
          maxLength: 256
        },
        paths: {
          type: "array",
          items: {
            type: "string",
            maxLength: 2048
          },
          maxItems: 128
        },
        kind: {
          enum: [
            "existing",
            "normal",
            "internal",
            "task"
          ]
        },
        position: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        recencyAt: {
          anyOf: [
            {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "nativeId",
        "source",
        "name",
        "paths"
      ],
      additionalProperties: false
    },
    StatusInput: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    NativeDiscoveryInput: {
      type: "object",
      properties: {
        query: {
          type: "string",
          maxLength: 512
        },
        method: {
          type: "string",
          maxLength: 192,
          minLength: 1
        }
      },
      required: [],
      additionalProperties: false
    },
    NativeDiscovery: {
      type: "object",
      properties: {
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        catalogHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        items: {
          type: "array",
          items: {
            type: "object",
            properties: {
              method: {
                type: "string",
                maxLength: 192,
                minLength: 1
              },
              description: {
                type: "string",
                maxLength: 2048
              },
              readOnlyHint: {
                type: "boolean"
              },
              expectedDefinitionHash: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
              },
              inputSchema: {
                anyOf: [
                  {
                    type: "boolean"
                  },
                  {
                    type: "object",
                    additionalProperties: {
                      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                    }
                  }
                ]
              },
              outputSchema: {
                anyOf: [
                  {
                    type: "boolean"
                  },
                  {
                    type: "object",
                    additionalProperties: {
                      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
                    }
                  }
                ]
              },
              runtimeConstraints: {
                type: "array",
                items: {
                  type: "object",
                  properties: {
                    condition: {
                      type: "string",
                      maxLength: 256
                    },
                    unsupportedParameters: {
                      type: "array",
                      items: {
                        type: "string",
                        maxLength: 128
                      },
                      maxItems: 32
                    },
                    guidance: {
                      type: "string",
                      maxLength: 1024
                    }
                  },
                  required: [
                    "condition",
                    "unsupportedParameters",
                    "guidance"
                  ],
                  additionalProperties: false
                },
                maxItems: 32
              }
            },
            required: [
              "method",
              "description",
              "readOnlyHint",
              "expectedDefinitionHash"
            ],
            additionalProperties: false
          },
          maxItems: 256
        }
      },
      required: [
        "nativeVersion",
        "catalogHash",
        "items"
      ],
      additionalProperties: false
    },
    FrameLimitsInput: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    FrameLimits: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        nativeExecutableHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        catalogHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        epoch: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        requestFrameBytes: {
          const: 6291456
        },
        receivedFrameBytes: {
          const: 25165824
        },
        answerFrameBytes: {
          const: 4194304
        },
        managementFrameBytes: {
          const: 33554432
        }
      },
      required: [
        "serviceNodeId",
        "nativeVersion",
        "nativeExecutableHash",
        "catalogHash",
        "epoch",
        "requestFrameBytes",
        "receivedFrameBytes",
        "answerFrameBytes",
        "managementFrameBytes"
      ],
      additionalProperties: false
    },
    OperationInput: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "operationId"
      ],
      additionalProperties: false
    },
    Interaction: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        reply: {
          $ref: "#/$defs/Reply"
        }
      },
      required: [
        "operationId",
        "epoch",
        "reply"
      ],
      additionalProperties: false
    },
    PreventInput: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Caller-generated <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status, use current Unix milliseconds and a unique 1-128 character nonce without a colon. Valid for 24 hours and at most 60 seconds in the future; a Hive restart changes the epoch. Keep this exact ID and check agent_manager_read after an uncertain result."
        },
        nativeVersion: {
          enum: [
            "0.154.0",
            "0.158.0"
          ]
        },
        method: {
          type: "string",
          maxLength: 192,
          minLength: 1
        },
        params: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        expectedDefinitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "operationId",
        "nativeVersion",
        "method",
        "params",
        "expectedDefinitionHash"
      ],
      additionalProperties: false
    },
    OperationAbsence: {
      type: "object",
      properties: {
        kind: {
          const: "agent_operation_absent"
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        epoch: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "kind",
        "operationId",
        "serviceNodeId",
        "epoch"
      ],
      additionalProperties: false
    },
    PendingInputQuery: {
      type: "object",
      properties: {
        includeExpired: {
          type: "boolean"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 128
        },
        identity: {
          $ref: "#/$defs/InputIdentity"
        }
      },
      required: [],
      additionalProperties: false
    },
    PendingInputPage: {
      type: "object",
      properties: {
        epoch: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/PendingInput"
          },
          maxItems: 128
        },
        truncated: {
          type: "boolean"
        }
      },
      required: [
        "epoch",
        "items",
        "truncated"
      ],
      additionalProperties: false
    },
    InputDefinitionQuery: {
      type: "object",
      properties: {
        identity: {
          $ref: "#/$defs/InputIdentity"
        }
      },
      required: [
        "identity"
      ],
      additionalProperties: false
    },
    InputDefinition: {
      type: "object",
      properties: {
        identity: {
          $ref: "#/$defs/InputIdentity"
        },
        method: {
          type: "string",
          maxLength: 192,
          minLength: 1
        },
        nativeVersion: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        catalogHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        responseSchema: {
          anyOf: [
            {
              type: "boolean"
            },
            {
              type: "object",
              additionalProperties: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
              }
            }
          ]
        }
      },
      required: [
        "identity",
        "method",
        "nativeVersion",
        "catalogHash",
        "responseSchema"
      ],
      additionalProperties: false
    },
    AnswerInput: {
      type: "object",
      properties: {
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        identity: {
          $ref: "#/$defs/InputIdentity"
        },
        reply: {
          $ref: "#/$defs/Reply"
        }
      },
      required: [
        "operationId",
        "identity",
        "reply"
      ],
      additionalProperties: false
    },
    NotificationQuery: {
      type: "object",
      properties: {
        afterSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 100
        }
      },
      required: [
        "afterSequence"
      ],
      additionalProperties: false
    },
    NotificationPage: {
      type: "object",
      properties: {
        epoch: {
          anyOf: [
            {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            {
              type: "null"
            }
          ]
        },
        firstAvailableSequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        throughSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        gap: {
          type: "boolean"
        },
        hasMore: {
          type: "boolean"
        },
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/Notification"
          },
          maxItems: 100
        }
      },
      required: [
        "epoch",
        "firstAvailableSequence",
        "throughSequence",
        "gap",
        "hasMore",
        "items"
      ],
      additionalProperties: false
    },
    ProjectsInput: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    ProjectSelection: {
      oneOf: [
        {
          type: "object",
          properties: {
            kind: {
              const: "existing"
            },
            cwd: {
              type: "string",
              maxLength: 2048,
              minLength: 1
            }
          },
          required: [
            "kind",
            "cwd"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              enum: [
                "normal",
                "internal"
              ]
            },
            key: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            name: {
              type: "string",
              maxLength: 256,
              minLength: 1
            }
          },
          required: [
            "kind",
            "key",
            "name"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "task"
            },
            key: {
              type: "string",
              pattern: "^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"
            },
            name: {
              type: "string",
              maxLength: 256,
              minLength: 1
            }
          },
          required: [
            "kind",
            "key",
            "name"
          ],
          additionalProperties: false
        }
      ]
    },
    ProjectResolveInput: {
      type: "object",
      properties: {
        selection: {
          $ref: "#/$defs/ProjectSelection"
        },
        expectedProjectId: {
          type: "string",
          maxLength: 256,
          minLength: 1
        }
      },
      required: [
        "selection"
      ],
      additionalProperties: false
    },
    ProjectLocation: {
      type: "object",
      properties: {
        project: {
          $ref: "#/$defs/ProjectSummary"
        },
        cwd: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        kind: {
          enum: [
            "existing",
            "normal",
            "internal",
            "task"
          ]
        }
      },
      required: [
        "project",
        "cwd",
        "kind"
      ],
      additionalProperties: false
    },
    ProjectsResult: {
      type: "object",
      properties: {
        source: {
          enum: [
            "native",
            "host-configuration"
          ]
        },
        observedAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        },
        projects: {
          type: "array",
          items: {
            $ref: "#/$defs/ProjectSummary"
          },
          maxItems: 256
        },
        defaults: {
          type: "object",
          properties: {
            projectRoot: {
              type: "string",
              maxLength: 2048
            },
            internalProjectRoot: {
              type: "string",
              maxLength: 2048
            }
          },
          required: [
            "projectRoot",
            "internalProjectRoot"
          ],
          additionalProperties: false
        }
      },
      required: [
        "source",
        "observedAt",
        "projects"
      ],
      additionalProperties: false
    },
    WorkspaceRequirement: {
      oneOf: [
        {
          type: "object",
          properties: {
            kind: {
              const: "task_workspace"
            }
          },
          required: [
            "kind"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "directory_path"
            },
            path: {
              type: "string",
              maxLength: 2048,
              minLength: 1
            }
          },
          required: [
            "kind",
            "path"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "existing_project"
            },
            projectId: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            path: {
              anyOf: [
                {
                  type: "string",
                  maxLength: 2048
                },
                {
                  type: "null"
                }
              ]
            },
            useWorktree: {
              type: "boolean"
            }
          },
          required: [
            "kind",
            "projectId",
            "path",
            "useWorktree"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "repository_path"
            },
            repositoryUrl: {
              type: "string",
              maxLength: 4096,
              minLength: 1
            },
            folderName: {
              type: "string",
              maxLength: 128,
              minLength: 1
            }
          },
          required: [
            "kind",
            "repositoryUrl",
            "folderName"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            kind: {
              const: "new_project_path"
            },
            folderName: {
              type: "string",
              maxLength: 128,
              minLength: 1
            }
          },
          required: [
            "kind",
            "folderName"
          ],
          additionalProperties: false
        }
      ]
    },
    WorkspaceResolveInput: {
      type: "object",
      properties: {
        taskKey: {
          type: "string",
          maxLength: 256,
          minLength: 1,
          pattern: "^TASK-[0-9]{4,}$"
        },
        requirement: {
          $ref: "#/$defs/WorkspaceRequirement"
        },
        prepare: {
          type: "boolean"
        },
        verifyOrigin: {
          type: "boolean"
        }
      },
      required: [
        "taskKey",
        "requirement",
        "prepare"
      ],
      additionalProperties: false
    },
    RepositoryObservation: {
      type: "object",
      properties: {
        name: {
          type: "string",
          maxLength: 512
        },
        branch: {
          anyOf: [
            {
              type: "string",
              maxLength: 512
            },
            {
              type: "null"
            }
          ]
        },
        commit: {
          anyOf: [
            {
              type: "string",
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        dirty: {
          type: "boolean"
        },
        originName: {
          anyOf: [
            {
              type: "string",
              maxLength: 128
            },
            {
              type: "null"
            }
          ]
        },
        originUrl: {
          anyOf: [
            {
              type: "string",
              maxLength: 4096
            },
            {
              type: "null"
            }
          ]
        },
        originState: {
          enum: [
            "confirmed",
            "local_only",
            "no_origin",
            "unknown"
          ]
        },
        remoteRef: {
          anyOf: [
            {
              type: "string",
              maxLength: 1024
            },
            {
              type: "null"
            }
          ]
        },
        observedAt: {
          type: "string",
          maxLength: 64,
          minLength: 1
        },
        limitation: {
          anyOf: [
            {
              type: "string",
              maxLength: 4096
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "name",
        "branch",
        "commit",
        "dirty",
        "originName",
        "originUrl",
        "originState",
        "remoteRef",
        "observedAt",
        "limitation"
      ],
      additionalProperties: false
    },
    WorkspaceResolution: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        canonicalCwd: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        taskRoot: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        bootstrapPath: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        intendedPath: {
          type: "string",
          maxLength: 2048,
          minLength: 1
        },
        sourcePath: {
          anyOf: [
            {
              type: "string",
              maxLength: 2048
            },
            {
              type: "null"
            }
          ]
        },
        project: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
            },
            {
              type: "null"
            }
          ]
        },
        nativeProjectId: {
          type: "string",
          maxLength: 256,
          minLength: 1
        },
        useWorktree: {
          type: "boolean"
        },
        repository: {
          anyOf: [
            {
              $ref: "#/$defs/RepositoryObservation"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "hostId",
        "serviceNodeId",
        "canonicalCwd",
        "taskRoot",
        "bootstrapPath",
        "intendedPath",
        "sourcePath",
        "project",
        "useWorktree",
        "repository"
      ],
      additionalProperties: false
    },
    Read01540ThreadReadd0: {
      properties: {
        includeTurns: {
          description: "When true, include turns and their items from rollout history. Full-history hydration is deprecated for paginated threads; prefer a metadata-only read and page with `thread/turns/list` and `thread/items/list`.",
          type: "boolean"
        },
        threadId: {
          type: "string"
        }
      },
      required: [
        "threadId"
      ],
      type: "object",
      additionalProperties: false
    },
    Read01540ThreadTurnsListd0: {
      properties: {
        cursor: {
          description: "Opaque cursor to pass to the next call to continue after the last turn.",
          type: [
            "string",
            "null"
          ]
        },
        itemsView: {
          anyOf: [
            {
              $ref: "#/$defs/Read01540ThreadTurnsListd1"
            },
            {
              type: "null"
            }
          ],
          description: "How much item detail to include for each returned turn; defaults to summary."
        },
        limit: {
          description: "Optional turn page size.",
          format: "uint32",
          minimum: 0,
          type: [
            "integer",
            "null"
          ]
        },
        sortDirection: {
          anyOf: [
            {
              $ref: "#/$defs/Read01540ThreadTurnsListd2"
            },
            {
              type: "null"
            }
          ],
          description: "Optional turn pagination direction; defaults to descending."
        },
        threadId: {
          type: "string"
        }
      },
      required: [
        "threadId"
      ],
      type: "object",
      additionalProperties: false
    },
    Read01540ThreadTurnsListd1: {
      oneOf: [
        {
          description: "`items` was not loaded for this turn. The field is intentionally empty.",
          enum: [
            "notLoaded"
          ],
          type: "string"
        },
        {
          description: "`items` contains only a display summary for this turn.",
          enum: [
            "summary"
          ],
          type: "string"
        },
        {
          description: "`items` contains every ThreadItem available from persisted app-server history for this turn.",
          enum: [
            "full"
          ],
          type: "string"
        }
      ]
    },
    Read01540ThreadTurnsListd2: {
      enum: [
        "asc",
        "desc"
      ],
      type: "string"
    },
    Read01540ThreadItemsListd0: {
      properties: {
        cursor: {
          description: "Opaque cursor to pass to the next call to continue after the last item.",
          type: [
            "string",
            "null"
          ]
        },
        limit: {
          description: "Optional item page size.",
          format: "uint32",
          minimum: 0,
          type: [
            "integer",
            "null"
          ]
        },
        sortDirection: {
          anyOf: [
            {
              $ref: "#/$defs/Read01540ThreadItemsListd1"
            },
            {
              type: "null"
            }
          ],
          description: "Optional item pagination direction; defaults to ascending."
        },
        threadId: {
          type: "string"
        },
        turnId: {
          description: "Optional turn id to filter by. When omitted, returns items across the thread.",
          type: [
            "string",
            "null"
          ]
        }
      },
      required: [
        "threadId"
      ],
      type: "object",
      additionalProperties: false
    },
    Read01540ThreadItemsListd1: {
      enum: [
        "asc",
        "desc"
      ],
      type: "string"
    },
    Read01580ThreadReadd0: {
      properties: {
        includeTurns: {
          description: "When true, include turns and their items from rollout history. Full-history hydration is deprecated for paginated threads; prefer a metadata-only read and page with `thread/turns/list` and `thread/items/list`.",
          type: "boolean"
        },
        threadId: {
          type: "string"
        }
      },
      required: [
        "threadId"
      ],
      type: "object",
      additionalProperties: false
    },
    Read01580ThreadTurnsListd0: {
      properties: {
        cursor: {
          description: "Opaque cursor to pass to the next call to continue after the last turn.",
          type: [
            "string",
            "null"
          ]
        },
        itemsView: {
          anyOf: [
            {
              $ref: "#/$defs/Read01580ThreadTurnsListd1"
            },
            {
              type: "null"
            }
          ],
          description: "How much item detail to include for each returned turn; defaults to summary."
        },
        limit: {
          description: "Optional turn page size.",
          format: "uint32",
          minimum: 0,
          type: [
            "integer",
            "null"
          ]
        },
        sortDirection: {
          anyOf: [
            {
              $ref: "#/$defs/Read01580ThreadTurnsListd2"
            },
            {
              type: "null"
            }
          ],
          description: "Optional turn pagination direction; defaults to descending."
        },
        threadId: {
          type: "string"
        }
      },
      required: [
        "threadId"
      ],
      type: "object",
      additionalProperties: false
    },
    Read01580ThreadTurnsListd1: {
      oneOf: [
        {
          description: "`items` was not loaded for this turn. The field is intentionally empty.",
          enum: [
            "notLoaded"
          ],
          type: "string"
        },
        {
          description: "`items` contains only a display summary for this turn.",
          enum: [
            "summary"
          ],
          type: "string"
        },
        {
          description: "`items` contains every ThreadItem available from persisted app-server history for this turn.",
          enum: [
            "full"
          ],
          type: "string"
        }
      ]
    },
    Read01580ThreadTurnsListd2: {
      enum: [
        "asc",
        "desc"
      ],
      type: "string"
    },
    Read01580ThreadItemsListd0: {
      properties: {
        cursor: {
          description: "Opaque cursor to pass to the next call to continue after the last item.",
          type: [
            "string",
            "null"
          ]
        },
        limit: {
          description: "Optional item page size.",
          format: "uint32",
          minimum: 0,
          type: [
            "integer",
            "null"
          ]
        },
        sortDirection: {
          anyOf: [
            {
              $ref: "#/$defs/Read01580ThreadItemsListd1"
            },
            {
              type: "null"
            }
          ],
          description: "Optional item pagination direction; defaults to ascending."
        },
        threadId: {
          type: "string"
        },
        turnId: {
          description: "Optional turn id to filter by. When omitted, returns items across the thread.",
          type: [
            "string",
            "null"
          ]
        }
      },
      required: [
        "threadId"
      ],
      type: "object",
      additionalProperties: false
    },
    Read01580ThreadItemsListd1: {
      enum: [
        "asc",
        "desc"
      ],
      type: "string"
    },
    ReadInput: {
      oneOf: [
        {
          type: "object",
          properties: {
            nativeVersion: {
              const: "0.154.0"
            },
            method: {
              const: "thread/read"
            },
            params: {
              $ref: "#/$defs/Read01540ThreadReadd0"
            }
          },
          required: [
            "nativeVersion",
            "method",
            "params"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            nativeVersion: {
              const: "0.154.0"
            },
            method: {
              const: "thread/turns/list"
            },
            params: {
              $ref: "#/$defs/Read01540ThreadTurnsListd0"
            }
          },
          required: [
            "nativeVersion",
            "method",
            "params"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            nativeVersion: {
              const: "0.154.0"
            },
            method: {
              const: "thread/items/list"
            },
            params: {
              $ref: "#/$defs/Read01540ThreadItemsListd0"
            }
          },
          required: [
            "nativeVersion",
            "method",
            "params"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            nativeVersion: {
              const: "0.158.0"
            },
            method: {
              const: "thread/read"
            },
            params: {
              $ref: "#/$defs/Read01580ThreadReadd0"
            }
          },
          required: [
            "nativeVersion",
            "method",
            "params"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            nativeVersion: {
              const: "0.158.0"
            },
            method: {
              const: "thread/turns/list"
            },
            params: {
              $ref: "#/$defs/Read01580ThreadTurnsListd0"
            }
          },
          required: [
            "nativeVersion",
            "method",
            "params"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            nativeVersion: {
              const: "0.158.0"
            },
            method: {
              const: "thread/items/list"
            },
            params: {
              $ref: "#/$defs/Read01580ThreadItemsListd0"
            }
          },
          required: [
            "nativeVersion",
            "method",
            "params"
          ],
          additionalProperties: false
        }
      ]
    },
    ReadObservation: {
      oneOf: [
        {
          type: "object",
          properties: {
            schemaVersion: {
              const: 1
            },
            observationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            callerPrincipalId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            serviceNodeId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            nativeExecutableHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            catalogHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            epoch: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            requestId: {
              $ref: "#/$defs/RequestId"
            },
            observedAt: {
              type: "string",
              maxLength: 64,
              minLength: 1
            },
            requestHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            nativeVersion: {
              const: "0.154.0"
            },
            method: {
              const: "thread/read"
            },
            params: {
              $ref: "#/$defs/Read01540ThreadReadd0"
            },
            reply: {
              $ref: "#/$defs/Reply"
            }
          },
          required: [
            "schemaVersion",
            "observationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeExecutableHash",
            "catalogHash",
            "epoch",
            "requestId",
            "observedAt",
            "requestHash",
            "nativeVersion",
            "method",
            "params",
            "reply"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            schemaVersion: {
              const: 1
            },
            observationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            callerPrincipalId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            serviceNodeId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            nativeExecutableHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            catalogHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            epoch: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            requestId: {
              $ref: "#/$defs/RequestId"
            },
            observedAt: {
              type: "string",
              maxLength: 64,
              minLength: 1
            },
            requestHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            nativeVersion: {
              const: "0.154.0"
            },
            method: {
              const: "thread/turns/list"
            },
            params: {
              $ref: "#/$defs/Read01540ThreadTurnsListd0"
            },
            reply: {
              $ref: "#/$defs/Reply"
            }
          },
          required: [
            "schemaVersion",
            "observationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeExecutableHash",
            "catalogHash",
            "epoch",
            "requestId",
            "observedAt",
            "requestHash",
            "nativeVersion",
            "method",
            "params",
            "reply"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            schemaVersion: {
              const: 1
            },
            observationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            callerPrincipalId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            serviceNodeId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            nativeExecutableHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            catalogHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            epoch: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            requestId: {
              $ref: "#/$defs/RequestId"
            },
            observedAt: {
              type: "string",
              maxLength: 64,
              minLength: 1
            },
            requestHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            nativeVersion: {
              const: "0.154.0"
            },
            method: {
              const: "thread/items/list"
            },
            params: {
              $ref: "#/$defs/Read01540ThreadItemsListd0"
            },
            reply: {
              $ref: "#/$defs/Reply"
            }
          },
          required: [
            "schemaVersion",
            "observationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeExecutableHash",
            "catalogHash",
            "epoch",
            "requestId",
            "observedAt",
            "requestHash",
            "nativeVersion",
            "method",
            "params",
            "reply"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            schemaVersion: {
              const: 1
            },
            observationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            callerPrincipalId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            serviceNodeId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            nativeExecutableHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            catalogHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            epoch: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            requestId: {
              $ref: "#/$defs/RequestId"
            },
            observedAt: {
              type: "string",
              maxLength: 64,
              minLength: 1
            },
            requestHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            nativeVersion: {
              const: "0.158.0"
            },
            method: {
              const: "thread/read"
            },
            params: {
              $ref: "#/$defs/Read01580ThreadReadd0"
            },
            reply: {
              $ref: "#/$defs/Reply"
            }
          },
          required: [
            "schemaVersion",
            "observationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeExecutableHash",
            "catalogHash",
            "epoch",
            "requestId",
            "observedAt",
            "requestHash",
            "nativeVersion",
            "method",
            "params",
            "reply"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            schemaVersion: {
              const: 1
            },
            observationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            callerPrincipalId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            serviceNodeId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            nativeExecutableHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            catalogHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            epoch: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            requestId: {
              $ref: "#/$defs/RequestId"
            },
            observedAt: {
              type: "string",
              maxLength: 64,
              minLength: 1
            },
            requestHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            nativeVersion: {
              const: "0.158.0"
            },
            method: {
              const: "thread/turns/list"
            },
            params: {
              $ref: "#/$defs/Read01580ThreadTurnsListd0"
            },
            reply: {
              $ref: "#/$defs/Reply"
            }
          },
          required: [
            "schemaVersion",
            "observationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeExecutableHash",
            "catalogHash",
            "epoch",
            "requestId",
            "observedAt",
            "requestHash",
            "nativeVersion",
            "method",
            "params",
            "reply"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            schemaVersion: {
              const: 1
            },
            observationId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            callerPrincipalId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            serviceNodeId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            nativeExecutableHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            catalogHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            epoch: {
              type: "string",
              maxLength: 256,
              minLength: 1
            },
            requestId: {
              $ref: "#/$defs/RequestId"
            },
            observedAt: {
              type: "string",
              maxLength: 64,
              minLength: 1
            },
            requestHash: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
            },
            nativeVersion: {
              const: "0.158.0"
            },
            method: {
              const: "thread/items/list"
            },
            params: {
              $ref: "#/$defs/Read01580ThreadItemsListd0"
            },
            reply: {
              $ref: "#/$defs/Reply"
            }
          },
          required: [
            "schemaVersion",
            "observationId",
            "callerPrincipalId",
            "serviceNodeId",
            "nativeExecutableHash",
            "catalogHash",
            "epoch",
            "requestId",
            "observedAt",
            "requestHash",
            "nativeVersion",
            "method",
            "params",
            "reply"
          ],
          additionalProperties: false
        }
      ]
    }
  }
};

// packages/contracts/src/native-versions.ts
var nativeVersions = Object.freeze([...agent_schema_default.$defs.PreventInput.properties.nativeVersion.enum]);

// packages/contracts/src/transport-validation.ts
var object = (value, message) => {
  requireThat(value !== null && typeof value === "object" && !Array.isArray(value), "invalid_frame", message);
  return value;
};
var text = (value, maximum = 256) => typeof value === "string" && value.length > 0 && value.length <= maximum;
var identifier = (value) => text(value, 256);
var hash = (value) => typeof value === "string" && /^sha256:[a-f0-9]{64}$/.test(value);
var serviceName = (value) => typeof value === "string" && value.length <= 64 && /^[a-z][a-z0-9-]*$/.test(value);
var contractVersion = (value) => typeof value === "string" && /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(value);
var exactKeys = (value, allowed) => Object.keys(value).every((key) => allowed.includes(key));
function validateProviderCallFrame(value) {
  const frame = object(value, "Expected one provider request envelope.");
  requireThat(frame["jsonrpc"] === "2.0" && frame["method"] === "provider.invoke" && identifier(frame["id"]) && exactKeys(frame, ["jsonrpc", "id", "method", "params"]), "invalid_frame", "Provider request correlation is invalid.");
  const params = object(frame["params"], "Provider request params must be an object.");
  requireThat(
    exactKeys(params, ["qualifiedName", "arguments", "definitionHash", "generation", "callerPrincipalId", "operationId"]) && text(params["qualifiedName"]) && Object.hasOwn(params, "arguments") && hash(params["definitionHash"]) && Number.isSafeInteger(params["generation"]) && Number(params["generation"]) >= 1 && identifier(params["callerPrincipalId"]) && (params["operationId"] === void 0 || identifier(params["operationId"])),
    "invalid_frame",
    "Provider request routing fields are invalid."
  );
}
function validateProviderNotificationFrame(value) {
  const frame = object(value, "Expected one subscribed provider notification envelope.");
  requireThat(
    frame["jsonrpc"] === "2.0" && frame["method"] === "notifications.provider" && exactKeys(frame, ["jsonrpc", "method", "params"]),
    "invalid_frame",
    "Subscribed provider notification envelope is invalid."
  );
  const params = object(frame["params"], "Subscribed provider notification params must be an object.");
  requireThat(
    exactKeys(params, ["namespace", "name", "version", "payload", "serviceNodeId", "generation"]) && serviceName(params["namespace"]) && text(params["name"], 192) && contractVersion(params["version"]) && Object.hasOwn(params, "payload") && identifier(params["serviceNodeId"]) && Number.isSafeInteger(params["generation"]) && Number(params["generation"]) >= 1,
    "invalid_frame",
    "Subscribed provider notification routing fields are invalid."
  );
}
function validateChangeNotificationFrame(value) {
  const frame = object(value, "Expected one UI invalidation envelope.");
  requireThat(frame["jsonrpc"] === "2.0" && frame["method"] === "notifications.changed" && exactKeys(frame, ["jsonrpc", "method", "params"]), "invalid_frame", "UI invalidation envelope is invalid.");
  const params = object(frame["params"], "UI invalidation params must be an object.");
  const scopes = params["scopes"];
  requireThat(
    exactKeys(params, ["scopes"]) && Array.isArray(scopes) && scopes.length <= 64 && new Set(scopes).size === scopes.length && scopes.every((scope) => typeof scope === "string" && scope.length <= 512 && /^(objects(?:\/[a-zA-Z0-9._/-]+)?|services|inventory|uis|system)$/.test(scope)),
    "invalid_frame",
    "UI invalidation scopes are invalid."
  );
}
function validateEventAvailabilityFrame(value) {
  const frame = object(value, "Expected one event availability envelope.");
  requireThat(
    frame["jsonrpc"] === "2.0" && frame["method"] === "events.available" && exactKeys(frame, ["jsonrpc", "method", "params"]),
    "invalid_frame",
    "Event availability envelope is invalid."
  );
  const params = object(frame["params"], "Event availability params must be an object.");
  requireThat(
    exactKeys(params, ["throughSequence"]) && Number.isSafeInteger(params["throughSequence"]) && Number(params["throughSequence"]) >= 1,
    "invalid_frame",
    "Event availability sequence is invalid."
  );
}

// packages/sdk/src/client.ts
function queryDocument(item) {
  const read = item.document;
  requireThat(
    read && read.object.id === item.objectId && read.revision.objectId === item.objectId && read.revision.revision === item.revision && read.revision.contractVersion === item.contractVersion,
    "invalid_frame",
    "Hive query returned no matching document."
  );
  return read;
}
var runtimeEpochs = /* @__PURE__ */ new WeakMap();
async function runtimeEpoch(client) {
  let pending = runtimeEpochs.get(client);
  if (!pending) {
    pending = client.request("system.status", {}).then((status) => status.runtimeEpoch);
    runtimeEpochs.set(client, pending);
    void pending.catch(() => {
      if (runtimeEpochs.get(client) === pending) runtimeEpochs.delete(client);
    });
  }
  return pending;
}
async function newOperationId(client, nonce) {
  return operationId(await runtimeEpoch(client), Date.now(), nonce);
}
var scopedOperationIds = /* @__PURE__ */ new WeakMap();
function scopedOperationId(client, scope) {
  const key = canonical(scope, 65536);
  let values = scopedOperationIds.get(client);
  if (!values) {
    values = /* @__PURE__ */ new Map();
    scopedOperationIds.set(client, values);
  }
  const now = Date.now();
  for (const [scope2, entry2] of values) {
    if (entry2.issuedAt + 24 * 60 * 60 * 1e3 > now) break;
    values.delete(scope2);
  }
  let entry = values.get(key);
  if (!entry) {
    const pending = newOperationId(client);
    entry = { issuedAt: now, pending };
    values.set(key, entry);
    void pending.catch(() => {
      if (values.get(key)?.pending === pending) values.delete(key);
    });
  }
  return entry.pending;
}
async function discover(client, qualifiedName, target = {}) {
  const separator = qualifiedName.indexOf(".");
  requireThat(
    separator > 0,
    "invalid_arguments",
    "Expected namespace and exact tool name."
  );
  const namespace = qualifiedName.slice(0, separator), name = qualifiedName.slice(separator + 1);
  const { interfaceVersion, ...selectors } = target;
  let cursor;
  do {
    const page = await client.request("tools.list", {
      namespace,
      ...selectors,
      namePrefix: name,
      ...cursor ? { cursor } : {}
    });
    const binding = page.items.find(
      (tool) => tool.qualifiedName === qualifiedName
    );
    if (binding) {
      requireThat(
        interfaceVersion === void 0 || binding.definition.interfaceVersion === interfaceVersion,
        "contract_version_conflict",
        "The selected provider exposes another namespace interface version."
      );
      return {
        ...binding,
        serviceNodeId: page.provider.node.serviceNodeId,
        ...selectors.resourceRef ? { resourceRef: selectors.resourceRef } : {}
      };
    }
    cursor = page.nextCursor ?? void 0;
  } while (cursor);
  throw new IvyError(
    "not_found",
    "The exact tool is not in the selected provider catalog."
  );
}
function callBound(client, binding, args, operationId2, options) {
  return client.request(
    "tools.call",
    {
      qualifiedName: binding.qualifiedName,
      serviceNodeId: binding.serviceNodeId,
      expectedDefinitionHash: binding.definitionHash,
      arguments: args,
      ...options?.expectedCallerPrincipalId === void 0 ? {} : { expectedCallerPrincipalId: options.expectedCallerPrincipalId },
      ...binding.resourceRef ? { resourceRef: binding.resourceRef } : {},
      ...operationId2 === void 0 ? {} : { operationId: operationId2 }
    },
    options
  );
}
var toolClients = /* @__PURE__ */ new WeakMap();
function freezeBinding(value) {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) return;
  for (const child of Object.values(value)) freezeBinding(child);
  Object.freeze(value);
}
var BoundToolClient = class {
  constructor(client, serviceNodeId, requirements, dynamicNamespace = null) {
    this.client = client;
    this.serviceNodeId = serviceNodeId;
    this.dynamicNamespace = dynamicNamespace;
    requireThat(
      serviceNodeId.length > 0,
      "invalid_arguments",
      "A bound Tool client requires one exact service node."
    );
    requireThat(
      requirements.length > 0 || dynamicNamespace !== null,
      "invalid_arguments",
      "A bound Tool client requires an interface version or an explicit native namespace."
    );
    for (const requirement of requirements)
      requireThat(
        /^[a-z][a-z0-9-]{0,63}$/.test(requirement.namespace) && /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(
          requirement.interfaceVersion
        ),
        "invalid_arguments",
        "A bound Tool interface requirement is invalid."
      );
    this.interfaces = new Map(
      requirements.map((requirement) => [
        requirement.namespace,
        requirement.interfaceVersion
      ])
    );
    requireThat(
      this.interfaces.size === requirements.length,
      "invalid_arguments",
      "A bound Tool client cannot declare the same namespace twice."
    );
  }
  client;
  serviceNodeId;
  dynamicNamespace;
  bindings = /* @__PURE__ */ new Map();
  interfaces;
  async binding(qualifiedName, expectedDefinitionHash) {
    let pending = this.bindings.get(qualifiedName);
    if (!pending) {
      requireThat(
        this.bindings.size < 256,
        "limit_exceeded",
        "The selected provider binding cache is full."
      );
      const separator = qualifiedName.indexOf(".");
      requireThat(
        separator > 0,
        "invalid_arguments",
        "Expected namespace and exact tool name."
      );
      const namespace = qualifiedName.slice(0, separator), interfaceVersion = this.interfaces.get(namespace);
      requireThat(
        interfaceVersion !== void 0 || namespace === this.dynamicNamespace,
        "invalid_arguments",
        "The Tool namespace has no declared interface requirement."
      );
      pending = discover(this.client, qualifiedName, {
        serviceNodeId: this.serviceNodeId,
        ...interfaceVersion ? { interfaceVersion } : {}
      }).then((binding2) => {
        requireThat(
          binding2.serviceNodeId === this.serviceNodeId,
          "target_conflict",
          "Discovery returned another selected provider."
        );
        freezeBinding(binding2);
        return binding2;
      });
      this.bindings.set(qualifiedName, pending);
      void pending.catch(() => {
        if (this.bindings.get(qualifiedName) === pending)
          this.bindings.delete(qualifiedName);
      });
    }
    const binding = await pending;
    requireThat(
      expectedDefinitionHash === void 0 || binding.definitionHash === expectedDefinitionHash,
      "tool_definition_changed",
      "The selected Tool differs from the retained action definition."
    );
    return binding;
  }
  async call(qualifiedName, args, operationId2, options) {
    options?.signal?.throwIfAborted();
    const binding = await this.binding(qualifiedName);
    options?.signal?.throwIfAborted();
    return callBound(this.client, binding, args, operationId2, options);
  }
};
function selectedTools(client, serviceNodeId, requirements, dynamicNamespace) {
  let providers = toolClients.get(client);
  if (!providers) {
    providers = /* @__PURE__ */ new Map();
    toolClients.set(client, providers);
  }
  const key = serviceNodeId + "\0" + canonical({
    requirements: [...requirements].sort(
      (a, b) => a.namespace.localeCompare(b.namespace)
    ),
    dynamicNamespace
  });
  let selected = providers.get(key);
  if (!selected) {
    requireThat(
      providers.size < 64,
      "limit_exceeded",
      "The RPC client has too many selected Tool providers."
    );
    selected = new BoundToolClient(
      client,
      serviceNodeId,
      requirements,
      dynamicNamespace
    );
    providers.set(key, selected);
  }
  return selected;
}
function serviceTools(client, serviceNodeId, requirements) {
  requireThat(
    requirements.length > 0,
    "invalid_arguments",
    "Stable service Tools require at least one namespace interface version."
  );
  return selectedTools(client, serviceNodeId, requirements, null);
}
function nativeServiceTools(client, serviceNodeId) {
  return selectedTools(client, serviceNodeId, [], "codex");
}
function responseValue(frame, id) {
  if (frame === null || typeof frame !== "object" || Array.isArray(frame))
    throw new IvyError(
      "invalid_frame",
      "Hive returned an invalid response.",
      "unknown"
    );
  const value = frame;
  if (value["jsonrpc"] !== "2.0" || "error" in value === "result" in value)
    throw new IvyError(
      "invalid_frame",
      "Hive response envelope is invalid.",
      "unknown"
    );
  const result = "result" in value;
  if (!Object.keys(value).every(
    (key) => (result ? ["jsonrpc", "id", "result"] : ["jsonrpc", "id", "error"]).includes(key)
  ))
    throw new IvyError(
      "invalid_frame",
      "Hive response envelope contains unexpected fields.",
      "unknown"
    );
  if ("result" in value) {
    if (value["id"] !== id)
      throw new IvyError(
        "invalid_frame",
        "Hive response identity is invalid.",
        "unknown"
      );
    return value["result"];
  }
  const error = value["error"];
  if (!error || !Object.keys(error).every(
    (key) => ["code", "message", "data"].includes(key)
  ) || !Number.isSafeInteger(error.code) || typeof error.message !== "string" || error.message.length < 1 || error.message.length > 2048 || !error.data || !Object.keys(error.data).every(
    (key) => ["code", "outcome", "details"].includes(key)
  ) || typeof error.data.code !== "string" || error.data.code.length < 1 || error.data.code.length > 128 || !["not_executed", "completed", "unknown"].includes(error.data.outcome))
    throw new IvyError(
      "invalid_frame",
      "Hive returned an invalid error.",
      "unknown"
    );
  if (value["id"] !== id && !(value["id"] === null && error.data.outcome === "not_executed"))
    throw new IvyError(
      "invalid_frame",
      "Hive error identity is invalid.",
      "unknown"
    );
  throw new IvyError(
    error.data.code,
    error.message,
    error.data.outcome,
    error.data.details
  );
}
function baseUrl(value) {
  const url = new URL(value);
  requireThat(
    ["https:", "http:"].includes(url.protocol) && !url.username && !url.password && !url.search && !url.hash,
    "invalid_arguments",
    "Expected the canonical Hive base URL."
  );
  requireThat(
    url.protocol === "https:" || ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname),
    "invalid_arguments",
    "Remote Hive connections require HTTPS."
  );
  url.pathname = url.pathname.replace(/\/$/, "") + "/";
  return url;
}
var HiveClient = class {
  constructor(value, options = {}) {
    this.options = options;
    this.base = baseUrl(value);
    const maximum = options.requestCalls ?? connectionInFlightRequests;
    requireThat(
      Number.isSafeInteger(maximum) && maximum > 0 && maximum <= connectionInFlightRequests,
      "invalid_arguments",
      "Invalid client concurrency limit."
    );
    this.queue = options.requestCalls || options.providerCalls ? new RequestQueue(
      maximum,
      options.providerCalls ?? maximum,
      options.queuedRequests ?? 128
    ) : null;
  }
  options;
  base;
  active = 0;
  queue;
  async request(method, params, options = {}) {
    const controller = new AbortController();
    const timer = setTimeout(
      () => controller.abort(),
      options.timeoutMs ?? 35e3
    );
    const signal = options.signal ? AbortSignal.any([controller.signal, options.signal]) : controller.signal;
    let release;
    let sent = false;
    try {
      signal.throwIfAborted();
      if (this.queue)
        release = await this.queue.acquire(
          method === "tools.call" || method === "discovery.call",
          signal
        );
      signal.throwIfAborted();
      sent = true;
      return await this.send(method, params, { ...options, signal });
    } catch (error) {
      if (!sent && controller.signal.aborted && !options.signal?.aborted)
        throw new IvyError(
          "deadline_exceeded",
          "Hive request expired before sending.",
          "not_executed"
        );
      throw error;
    } finally {
      release?.();
      clearTimeout(timer);
    }
  }
  async send(method, params, options) {
    requireThat(
      this.active < connectionInFlightRequests,
      "limit_exceeded",
      "Hive client request limit reached."
    );
    options.signal?.throwIfAborted();
    const id = crypto.randomUUID(), body = encodeJson({ jsonrpc: "2.0", id, method, params });
    const signal = options.signal;
    this.active++;
    try {
      const response = await (this.options.fetch ?? fetch)(
        new URL("api/v1/rpc", this.base),
        {
          method: "POST",
          credentials: "same-origin",
          redirect: "error",
          signal,
          headers: {
            "Content-Type": "application/json",
            ...this.options.credential ? { Authorization: "Bearer " + this.options.credential } : {}
          },
          body
        }
      );
      const reader = response.body?.getReader();
      if (!reader)
        throw new IvyError(
          "invalid_frame",
          "Hive returned no response body.",
          "unknown"
        );
      const chunks = [];
      let size = 0;
      try {
        for (; ; ) {
          const part = await reader.read();
          if (part.done) break;
          size += part.value.byteLength;
          if (size > managementFrameBytes) {
            await reader.cancel();
            throw new IvyError(
              "result_too_large",
              "Hive response exceeds its frame limit.",
              "unknown"
            );
          }
          chunks.push(part.value);
        }
      } finally {
        reader.releaseLock();
      }
      const buffer = new Uint8Array(size);
      let offset = 0;
      for (const chunk of chunks) {
        buffer.set(chunk, offset);
        offset += chunk.byteLength;
      }
      let frame;
      try {
        frame = JSON.parse(
          new TextDecoder("utf-8", { fatal: true }).decode(buffer)
        );
      } catch {
        throw new IvyError(
          "invalid_frame",
          "Hive returned malformed JSON.",
          "unknown"
        );
      }
      return responseValue(frame, id);
    } catch (error) {
      if (error instanceof IvyError) throw error;
      throw new IvyError(
        "outcome_unknown",
        "The request connection failed or timed out. Reconcile its original operation identity.",
        "unknown"
      );
    } finally {
      this.active--;
    }
  }
};
function browserClient(value) {
  const base = baseUrl(value);
  requireThat(
    base.origin === location.origin,
    "invalid_arguments",
    "Browser Hive access must stay on the current origin."
  );
  return new HiveClient(base.href, {
    requestCalls: 16,
    providerCalls: 8,
    queuedRequests: 128
  });
}
var BrowserNotifications = class {
  base;
  listeners = /* @__PURE__ */ new Map();
  socket = null;
  changeListeners = /* @__PURE__ */ new Map();
  statusListeners = /* @__PURE__ */ new Set();
  ready = false;
  deadline;
  heartbeat;
  reconnect;
  reconnectMs = 500;
  pending = null;
  syncedSignature = null;
  constructor(value) {
    this.base = baseUrl(value);
    requireThat(
      this.base.origin === location.origin,
      "invalid_arguments",
      "Browser Hive notifications must stay on the current origin."
    );
  }
  subscribe(filter, listener) {
    this.listeners.set(listener, structuredClone(filter));
    this.changed();
    return () => {
      this.listeners.delete(listener);
      this.changed();
    };
  }
  get connected() {
    return this.ready;
  }
  onStatus(listener) {
    this.statusListeners.add(listener);
    listener(this.ready);
    return () => {
      this.statusListeners.delete(listener);
    };
  }
  subscribeChanges(scopes, listener) {
    this.changeListeners.set(listener, [...scopes]);
    this.changed();
    return () => {
      this.changeListeners.delete(listener);
      this.changed();
    };
  }
  /** Resume after browser suspension or a network change with a fresh subscription. */
  reconnectNow() {
    if (this.socket) this.disconnected(this.socket);
    clearTimeout(this.reconnect);
    this.connect();
  }
  setReady(value) {
    if (this.ready === value) return;
    this.ready = value;
    for (const listener of this.statusListeners) {
      try {
        listener(value);
      } catch {
      }
    }
  }
  wanted() {
    return this.listeners.size + this.changeListeners.size > 0;
  }
  changed() {
    if (!this.changeListeners.size) clearTimeout(this.heartbeat);
    if (this.wanted()) {
      this.connect();
      this.sync();
      return;
    }
    if (this.socket) this.disconnected(this.socket);
    clearTimeout(this.reconnect);
    this.reconnect = void 0;
  }
  disconnected(socket) {
    if (this.socket !== socket) return;
    this.socket = null;
    this.pending = null;
    this.syncedSignature = null;
    clearTimeout(this.deadline);
    clearTimeout(this.heartbeat);
    this.setReady(false);
    socket.close();
    this.retry();
  }
  retry() {
    if (!this.wanted()) return;
    clearTimeout(this.reconnect);
    const wait = this.reconnectMs;
    this.reconnectMs = Math.min(1e4, this.reconnectMs * 2);
    this.reconnect = setTimeout(() => this.connect(), wait);
  }
  filters() {
    const unique = /* @__PURE__ */ new Map();
    for (const filter of this.listeners.values())
      unique.set(
        `${filter.namespace}\0${filter.name}\0${filter.version}\0${filter.serviceNodeId ?? ""}`,
        filter
      );
    return [...unique.values()];
  }
  sync() {
    const socket = this.socket;
    if (!this.wanted() || !socket || socket.readyState !== WebSocket.OPEN || this.pending)
      return;
    const filters = this.filters(), changes = [...new Set([...this.changeListeners.values()].flat())].sort(), signature = canonical({ filters, changes });
    if (signature === this.syncedSignature) return;
    const id = crypto.randomUUID();
    this.setReady(false);
    this.pending = {
      id,
      signature,
      count: filters.length,
      changes: changes.length
    };
    clearTimeout(this.deadline);
    this.deadline = setTimeout(() => this.disconnected(socket), 5e3);
    try {
      socket.send(
        encodeJson({
          jsonrpc: "2.0",
          id,
          method: "notifications.subscribe",
          params: { filters, ...changes.length ? { changes } : {} }
        })
      );
    } catch {
      this.disconnected(socket);
    }
  }
  connect() {
    if (!this.wanted() || this.socket && (this.socket.readyState === WebSocket.CONNECTING || this.socket.readyState === WebSocket.OPEN))
      return;
    clearTimeout(this.reconnect);
    this.reconnect = void 0;
    const url = new URL("ws", this.base);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    let socket;
    try {
      socket = new WebSocket(url);
    } catch {
      this.retry();
      return;
    }
    this.socket = socket;
    this.deadline = setTimeout(() => this.disconnected(socket), 1e4);
    socket.addEventListener("open", () => {
      if (this.socket === socket) {
        this.sync();
      }
    });
    socket.addEventListener("message", (event) => {
      if (this.socket !== socket || typeof event.data !== "string") return;
      if (this.changeListeners.size) {
        clearTimeout(this.heartbeat);
        this.heartbeat = setTimeout(() => this.disconnected(socket), 35e3);
      }
      let frame;
      try {
        frame = JSON.parse(event.data);
      } catch {
        this.disconnected(socket);
        return;
      }
      if (frame && typeof frame === "object" && !Array.isArray(frame) && "id" in frame) {
        const pending = this.pending;
        if (!pending || String(frame["id"]) !== pending.id) {
          this.disconnected(socket);
          return;
        }
        try {
          const result = responseValue(frame, pending.id);
          requireThat(
            result !== null && typeof result === "object" && !Array.isArray(result) && Object.keys(result).length === (pending.changes ? 2 : 1) && (!pending.changes || result["changes"] === pending.changes) && result["subscribed"] === pending.count,
            "invalid_frame",
            "Hive returned an invalid notification subscription result."
          );
          this.syncedSignature = pending.signature;
          this.pending = null;
          clearTimeout(this.deadline);
          this.reconnectMs = 500;
          this.sync();
          if (this.socket === socket && !this.pending) this.setReady(true);
        } catch {
          this.disconnected(socket);
        }
        return;
      }
      if (frame && typeof frame === "object" && "method" in frame && frame.method === "notifications.changed") {
        try {
          validateChangeNotificationFrame(frame);
        } catch {
          this.disconnected(socket);
          return;
        }
        const scopes = frame.params.scopes;
        for (const [listener, watched] of this.changeListeners) {
          const selected = scopes.filter(
            (scope) => watched.some(
              (value) => scope === value || scope.startsWith(value + "/") || value.startsWith(scope + "/")
            )
          );
          if (selected.length) {
            try {
              listener(selected);
            } catch {
            }
          }
        }
        return;
      }
      try {
        validateProviderNotificationFrame(frame);
      } catch {
        this.disconnected(socket);
        return;
      }
      for (const [listener, filter] of this.listeners) {
        if (filter.namespace !== frame.params.namespace || filter.name !== frame.params.name || filter.version !== frame.params.version || filter.serviceNodeId !== void 0 && filter.serviceNodeId !== frame.params.serviceNodeId)
          continue;
        try {
          listener(frame);
        } catch {
        }
      }
    });
    socket.addEventListener("close", () => {
      this.disconnected(socket);
    });
    socket.addEventListener("error", () => {
      this.disconnected(socket);
    });
  }
};
function browserNotifications(value) {
  return new BrowserNotifications(value);
}

export {
  operationId,
  parseOperationId,
  deriveOperationId,
  agent_schema_default,
  nativeVersions,
  validateProviderCallFrame,
  validateProviderNotificationFrame,
  validateEventAvailabilityFrame,
  queryDocument,
  runtimeEpoch,
  newOperationId,
  scopedOperationId,
  discover,
  callBound,
  BoundToolClient,
  serviceTools,
  nativeServiceTools,
  responseValue,
  baseUrl,
  HiveClient,
  browserClient,
  BrowserNotifications,
  browserNotifications
};
//# sourceMappingURL=chunk-PJQS6QVQ.js.map
