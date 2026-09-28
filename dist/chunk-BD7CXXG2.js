import {
  hive_wire_schema_default,
  wireSchema
} from "./chunk-4KEVFZLT.js";
import {
  canonical,
  requireThat
} from "./chunk-T2KPXKB3.js";

// specs/schemas/hive-operations.schema.json
var hive_operations_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/hive-operations.schema.json",
  title: "Ivy canonical operation parameters and results",
  $defs: {
    ObjectMetadata: {
      type: "object",
      properties: {
        id: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        parentId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        ownerObjectId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        name: {
          type: "string",
          minLength: 1,
          maxLength: 255
        },
        path: {
          type: "string",
          minLength: 1,
          maxLength: 16384
        },
        position: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        icon: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 32
            },
            {
              type: "null"
            }
          ]
        },
        contractKey: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        currentRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        contractVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        archivedAt: {
          anyOf: [
            {
              type: "string",
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
            },
            {
              type: "null"
            }
          ]
        },
        effectivelyArchived: {
          type: "boolean"
        },
        createdAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        updatedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        }
      },
      required: [
        "id",
        "parentId",
        "ownerObjectId",
        "name",
        "path",
        "position",
        "icon",
        "contractKey",
        "currentRevision",
        "contractVersion",
        "archivedAt",
        "effectivelyArchived",
        "createdAt",
        "updatedAt"
      ],
      additionalProperties: false
    },
    RevisionMetadata: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        contractVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        byteLength: {
          type: "integer",
          minimum: 0,
          maximum: 8388608
        },
        createdAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        references: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/RevisionReferences"
        }
      },
      required: [
        "objectId",
        "revision",
        "contractVersion",
        "contentHash",
        "mediaType",
        "byteLength",
        "createdAt",
        "references"
      ],
      additionalProperties: false
    },
    ObjectRead: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectMetadata"
        },
        revision: {
          $ref: "#/$defs/RevisionMetadata"
        },
        content: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Content"
        }
      },
      required: [
        "object",
        "revision",
        "content"
      ],
      additionalProperties: false
    },
    ObjectWriteResult: {
      type: "object",
      properties: {
        object: {
          $ref: "#/$defs/ObjectMetadata"
        },
        revision: {
          $ref: "#/$defs/RevisionMetadata"
        }
      },
      required: [
        "object",
        "revision"
      ],
      additionalProperties: false
    },
    QueryItem: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        contractVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        values: {
          type: "object",
          maxProperties: 40,
          additionalProperties: {
            type: [
              "null",
              "boolean",
              "number",
              "string"
            ]
          }
        },
        document: {
          $ref: "#/$defs/ObjectRead"
        }
      },
      required: [
        "objectId",
        "revision",
        "contractVersion",
        "values"
      ],
      additionalProperties: false
    },
    ServiceNode: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        instanceMode: {
          enum: [
            "singleton",
            "multiple"
          ]
        },
        principalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        version: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        hiveProtocol: {
          const: 1
        },
        nativeVersion: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "null"
            }
          ]
        },
        connected: {
          type: "boolean"
        },
        synced: {
          type: "boolean"
        },
        ready: {
          type: "boolean"
        },
        stale: {
          type: "boolean"
        },
        desiredEnabled: {
          type: "boolean"
        },
        lastContactAt: {
          anyOf: [
            {
              type: "string",
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
            },
            {
              type: "null"
            }
          ]
        },
        lastSuccessfulSyncAt: {
          anyOf: [
            {
              type: "string",
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
            },
            {
              type: "null"
            }
          ]
        },
        lastFailedSyncAt: {
          anyOf: [
            {
              type: "string",
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
            },
            {
              type: "null"
            }
          ]
        },
        lastObservationAt: {
          anyOf: [
            {
              type: "string",
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
            },
            {
              type: "null"
            }
          ]
        },
        diagnostic: {
          anyOf: [
            {
              type: "object",
              properties: {
                code: {
                  type: "string",
                  minLength: 1,
                  maxLength: 128
                },
                severity: {
                  enum: [
                    "info",
                    "warning",
                    "error"
                  ]
                },
                message: {
                  type: "string",
                  minLength: 1,
                  maxLength: 2048
                },
                observedAt: {
                  type: "string",
                  pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
                }
              },
              required: [
                "code",
                "severity",
                "message",
                "observedAt"
              ],
              additionalProperties: false
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
        "serviceName",
        "instanceMode",
        "principalId",
        "version",
        "buildId",
        "hiveProtocol",
        "nativeVersion",
        "connected",
        "synced",
        "ready",
        "stale",
        "desiredEnabled",
        "lastContactAt",
        "lastSuccessfulSyncAt",
        "lastFailedSyncAt",
        "lastObservationAt",
        "diagnostic"
      ],
      additionalProperties: false
    },
    Host: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceNodeIds: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
          },
          maxItems: 200
        },
        connected: {
          type: "boolean"
        }
      },
      required: [
        "hostId",
        "serviceNodeIds",
        "connected"
      ],
      additionalProperties: false
    },
    Provider: {
      type: "object",
      properties: {
        node: {
          $ref: "#/$defs/ServiceNode"
        },
        eligible: {
          type: "boolean"
        }
      },
      required: [
        "node",
        "eligible"
      ],
      additionalProperties: false
    },
    NodeContracts: {
      type: "object",
      properties: {
        provider: {
          $ref: "#/$defs/Provider"
        },
        hasCatalog: {
          type: "boolean"
        },
        capturedAt: {
          anyOf: [
            {
              type: "string",
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
            },
            {
              type: "null"
            }
          ]
        },
        items: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractRequirement"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "provider",
        "hasCatalog",
        "capturedAt",
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    ContractSummary: {
      type: "object",
      properties: {
        key: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        version: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        description: {
          type: "string",
          minLength: 1,
          maxLength: 512
        },
        owner: {
          oneOf: [
            {
              type: "object",
              properties: {
                kind: {
                  const: "hive"
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
                  const: "agent"
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
                  const: "service"
                },
                serviceName: {
                  type: "string",
                  minLength: 1,
                  maxLength: 64
                }
              },
              required: [
                "kind",
                "serviceName"
              ],
              additionalProperties: false
            }
          ]
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        hasJsonSchema: {
          type: "boolean"
        }
      },
      required: [
        "key",
        "version",
        "description",
        "owner",
        "mediaType",
        "hasJsonSchema"
      ],
      additionalProperties: false
    },
    ToolBinding: {
      type: "object",
      properties: {
        qualifiedName: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        definition: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ToolDefinition"
        },
        definitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash",
          description: "Hash of the exact returned tool definition; pass it unchanged when invoking that definition."
        }
      },
      required: [
        "qualifiedName",
        "definition",
        "definitionHash"
      ],
      additionalProperties: false
    },
    NamespaceSnapshot: {
      type: "object",
      properties: {
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        description: {
          type: "string",
          minLength: 1,
          maxLength: 8192
        },
        guideMarkdown: {
          type: "string",
          maxLength: 65536
        },
        provider: {
          $ref: "#/$defs/Provider"
        },
        toolCount: {
          type: "integer",
          minimum: 0,
          maximum: 2e3
        },
        topicCount: {
          type: "integer",
          minimum: 0,
          maximum: 128
        }
      },
      required: [
        "namespace",
        "description",
        "guideMarkdown",
        "provider",
        "toolCount",
        "topicCount"
      ],
      additionalProperties: false
    },
    NamespaceSummary: {
      type: "object",
      properties: {
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        providers: {
          type: "array",
          items: {
            $ref: "#/$defs/Provider"
          },
          maxItems: 200
        }
      },
      required: [
        "namespace",
        "providers"
      ],
      additionalProperties: false
    },
    DiscoveryEntry: {
      type: "object",
      properties: {
        kind: {
          enum: [
            "service",
            "group",
            "tool",
            "provider"
          ]
        },
        mcpName: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          description: "Direct MCP tool when available; call it with its published schema."
        },
        name: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        description: {
          type: "string",
          minLength: 1,
          maxLength: 320
        },
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        group: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        toolCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        providerCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        availableCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        available: {
          type: "boolean"
        },
        effect: {
          enum: [
            "read",
            "write",
            "unknown"
          ]
        }
      },
      required: [
        "kind",
        "name",
        "description",
        "serviceName"
      ],
      additionalProperties: false
    },
    DiscoveryProvider: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        available: {
          type: "boolean"
        }
      },
      required: [
        "serviceNodeId",
        "hostId",
        "available"
      ],
      additionalProperties: false
    },
    InventoryItem: {
      type: "object",
      properties: {
        resourceRef: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
        },
        schemaVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        summary: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        observedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        stale: {
          type: "boolean"
        },
        snapshotRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "resourceRef",
        "schemaVersion",
        "summary",
        "observedAt",
        "stale",
        "snapshotRevision"
      ],
      additionalProperties: false
    },
    Event: {
      type: "object",
      properties: {
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        topic: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        topicVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        source: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        occurredAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        payload: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        }
      },
      required: [
        "sequence",
        "topic",
        "topicVersion",
        "source",
        "occurredAt",
        "mutationId",
        "payload"
      ],
      additionalProperties: false
    },
    EventPublication: {
      type: "object",
      properties: {
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        topic: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        publishedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        }
      },
      required: [
        "sequence",
        "topic",
        "publishedAt"
      ],
      additionalProperties: false
    },
    EventGap: {
      type: "object",
      properties: {
        prunedThroughSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        resumeAfterSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "prunedThroughSequence",
        "resumeAfterSequence"
      ],
      additionalProperties: false
    },
    EventBatch: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/Event"
          },
          maxItems: 100
        },
        throughSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        hasMore: {
          type: "boolean"
        },
        gap: {
          anyOf: [
            {
              $ref: "#/$defs/EventGap"
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "throughSequence",
        "hasMore",
        "gap"
      ],
      additionalProperties: false
    },
    FamilyRetentionStatus: {
      type: "object",
      properties: {
        contractKey: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        policy: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/RetentionPolicy"
        },
        policyHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        previewRequired: {
          type: "boolean"
        },
        objectCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        revisionCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        byteLength: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        eligibleObjectCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        eligibleRevisionCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        protectedRevisionCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "contractKey",
        "policy",
        "policyHash",
        "previewRequired",
        "objectCount",
        "revisionCount",
        "byteLength",
        "eligibleObjectCount",
        "eligibleRevisionCount",
        "protectedRevisionCount"
      ],
      additionalProperties: false
    },
    RetentionStatus: {
      type: "object",
      properties: {
        checkedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        nextPassAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        lastSuccessAt: {
          anyOf: [
            {
              type: "string",
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
            },
            {
              type: "null"
            }
          ]
        },
        lastFailure: {
          anyOf: [
            {
              type: "object",
              properties: {
                at: {
                  type: "string",
                  pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
                },
                message: {
                  type: "string",
                  minLength: 1,
                  maxLength: 512
                }
              },
              required: [
                "at",
                "message"
              ],
              additionalProperties: false
            },
            {
              type: "null"
            }
          ]
        },
        families: {
          type: "array",
          items: {
            $ref: "#/$defs/FamilyRetentionStatus"
          },
          maxItems: 1024
        },
        events: {
          type: "object",
          properties: {
            count: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            byteLength: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            prunedThroughSequence: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            }
          },
          required: [
            "count",
            "byteLength",
            "prunedThroughSequence"
          ],
          additionalProperties: false
        },
        mutations: {
          type: "object",
          properties: {
            count: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            byteLength: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            expiredBefore: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            }
          },
          required: [
            "count",
            "byteLength",
            "expiredBefore"
          ],
          additionalProperties: false
        }
      },
      required: [
        "checkedAt",
        "nextPassAt",
        "lastSuccessAt",
        "lastFailure",
        "families",
        "events",
        "mutations"
      ],
      additionalProperties: false
    },
    UiMetadata: {
      type: "object",
      properties: {
        uiId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        displayName: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        description: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        iconKey: {
          enum: [
            "ui",
            "bot",
            "book-open",
            "list-checks",
            "message-circle",
            "phone",
            "camera",
            "newspaper",
            "settings",
            "layout-dashboard",
            "database",
            "user-round"
          ]
        }
      },
      required: [
        "uiId",
        "displayName",
        "description",
        "iconKey"
      ],
      additionalProperties: false
    },
    UiAsset: {
      type: "object",
      properties: {
        path: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 0,
          maximum: 8388608
        }
      },
      required: [
        "path",
        "mediaType",
        "contentHash",
        "byteLength"
      ],
      additionalProperties: false
    },
    ServiceRequirement: {
      type: "object",
      properties: {
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        interfaceVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        }
      },
      required: [
        "serviceName",
        "namespace",
        "interfaceVersion"
      ],
      additionalProperties: false
    },
    NotificationFilter: {
      type: "object",
      properties: {
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        name: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        version: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "namespace",
        "name",
        "version"
      ],
      additionalProperties: false
    },
    UiRequirements: {
      type: "object",
      properties: {
        hiveProtocol: {
          const: 1
        },
        contracts: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractRequirement"
          },
          maxItems: 128
        },
        services: {
          type: "array",
          items: {
            $ref: "#/$defs/ServiceRequirement"
          },
          maxItems: 128
        }
      },
      required: [
        "hiveProtocol",
        "contracts",
        "services"
      ],
      additionalProperties: false
    },
    UiRelease: {
      type: "object",
      properties: {
        releaseId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        entryPath: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        requirements: {
          $ref: "#/$defs/UiRequirements"
        },
        assets: {
          type: "array",
          items: {
            $ref: "#/$defs/UiAsset"
          },
          maxItems: 2e3
        }
      },
      required: [
        "releaseId",
        "entryPath",
        "requirements",
        "assets"
      ],
      additionalProperties: false
    },
    UI: {
      type: "object",
      properties: {
        metadata: {
          $ref: "#/$defs/UiMetadata"
        },
        currentReleaseId: {
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
        releases: {
          type: "array",
          items: {
            $ref: "#/$defs/UiRelease"
          },
          maxItems: 2e3
        }
      },
      required: [
        "metadata",
        "currentReleaseId",
        "releases"
      ],
      additionalProperties: false
    },
    UiReleaseSummary: {
      type: "object",
      properties: {
        releaseId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        entryPath: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        assetCount: {
          type: "integer",
          minimum: 0,
          maximum: 2e3
        }
      },
      required: [
        "releaseId",
        "entryPath",
        "assetCount"
      ],
      additionalProperties: false
    },
    UiIssue: {
      type: "object",
      properties: {
        code: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        message: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        resource: {
          type: "object",
          properties: {
            serviceNodeId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            serviceName: {
              type: "string",
              minLength: 1,
              maxLength: 64
            },
            namespace: {
              type: "string",
              minLength: 1,
              maxLength: 64
            },
            qualifiedName: {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            contractKey: {
              type: "string",
              minLength: 1,
              maxLength: 192
            },
            assetPath: {
              type: "string",
              minLength: 1,
              maxLength: 1024
            }
          },
          required: [],
          additionalProperties: false
        }
      },
      required: [
        "code",
        "message",
        "resource"
      ],
      additionalProperties: false
    },
    UiInspection: {
      type: "object",
      properties: {
        uiId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        currentReleaseId: {
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
        requestedReleaseId: {
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
        release: {
          anyOf: [
            {
              $ref: "#/$defs/UiReleaseSummary"
            },
            {
              type: "null"
            }
          ]
        },
        requirements: {
          anyOf: [
            {
              $ref: "#/$defs/UiRequirements"
            },
            {
              type: "null"
            }
          ]
        },
        checkedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        status: {
          enum: [
            "ready",
            "unavailable",
            "incompatible",
            "invalid",
            "missing",
            "unselected"
          ]
        },
        issues: {
          type: "array",
          items: {
            $ref: "#/$defs/UiIssue"
          },
          maxItems: 32
        },
        issuesTruncated: {
          type: "boolean"
        }
      },
      required: [
        "uiId",
        "currentReleaseId",
        "requestedReleaseId",
        "release",
        "requirements",
        "checkedAt",
        "status",
        "issues",
        "issuesTruncated"
      ],
      additionalProperties: false
    },
    UiSummary: {
      type: "object",
      properties: {
        metadata: {
          $ref: "#/$defs/UiMetadata"
        },
        currentReleaseId: {
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
        releaseCount: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        current: {
          $ref: "#/$defs/UiInspection"
        }
      },
      required: [
        "metadata",
        "currentReleaseId",
        "releaseCount",
        "current"
      ],
      additionalProperties: false
    },
    UiDefinition: {
      type: "object",
      properties: {
        metadata: {
          $ref: "#/$defs/UiMetadata"
        },
        entryPath: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        requirements: {
          $ref: "#/$defs/UiRequirements"
        },
        dataContracts: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DataContract"
          },
          maxItems: 128
        }
      },
      required: [
        "metadata",
        "entryPath",
        "requirements",
        "dataContracts"
      ],
      additionalProperties: false
    },
    UiPointerResult: {
      type: "object",
      properties: {
        uiId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        releaseId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        previousReleaseId: {
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
        "uiId",
        "releaseId",
        "previousReleaseId"
      ],
      additionalProperties: false
    },
    DeploymentSnapshot: {
      type: "object",
      properties: {
        record: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DeploymentRecord"
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        reportedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        stale: {
          type: "boolean"
        }
      },
      required: [
        "record",
        "serviceNodeId",
        "reportedAt",
        "stale"
      ],
      additionalProperties: false
    },
    HostConfigurationSummary: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        updatedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 1048576
        }
      },
      required: [
        "hostId",
        "objectId",
        "revision",
        "contentHash",
        "updatedAt",
        "byteLength"
      ],
      additionalProperties: false
    },
    HostConfiguration: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        updatedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 1048576
        },
        configuration: {
          $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/HostConfig"
        }
      },
      required: [
        "hostId",
        "objectId",
        "revision",
        "contentHash",
        "updatedAt",
        "byteLength",
        "configuration"
      ],
      additionalProperties: false
    },
    HostConfigurationEditor: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        updatedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        byteLength: {
          type: "integer",
          minimum: 1,
          maximum: 1048576
        },
        configuration: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        }
      },
      required: [
        "hostId",
        "objectId",
        "revision",
        "contentHash",
        "updatedAt",
        "byteLength",
        "configuration"
      ],
      additionalProperties: false
    },
    PackageUploadAuthorization: {
      type: "object",
      properties: {
        componentId: {
          type: "string",
          pattern: "^[a-z][a-z0-9-]*$",
          maxLength: 128,
          description: "Component ID from the prepared artifact manifest."
        },
        version: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion",
          description: "Semantic package version from the prepared artifact manifest; published versions are immutable and must increase."
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash",
          description: "SHA-256 build identity from the prepared artifact manifest and build stamp."
        },
        archiveHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash",
          description: "SHA-256 hash of the exact gzip tar archive to upload."
        },
        bytes: {
          type: "integer",
          minimum: 1,
          maximum: 268435456,
          description: "Exact archive size in bytes, from 1 through 268435456."
        },
        manifest: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json",
          description: "Exact validated release manifest of the prepared artifact; its componentId, version, and buildId must match these fields."
        }
      },
      required: [
        "componentId",
        "version",
        "buildId",
        "archiveHash",
        "bytes",
        "manifest"
      ],
      additionalProperties: false
    },
    PackageCatalogEntry: {
      type: "object",
      properties: {
        componentId: {
          type: "string",
          pattern: "^[a-z][a-z0-9-]*$",
          maxLength: 128
        },
        version: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        archiveHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        bytes: {
          type: "integer",
          minimum: 1,
          maximum: 268435456
        },
        manifest: {
          $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/ReleaseManifest"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        publishedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        publisherPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "componentId",
        "version",
        "buildId",
        "archiveHash",
        "bytes",
        "manifest",
        "revision",
        "publishedAt",
        "publisherPrincipalId"
      ],
      additionalProperties: false
    },
    PackageCatalog: {
      type: "object",
      properties: {
        schemaVersion: {
          const: 1
        },
        revision: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        packages: {
          type: "array",
          items: {
            $ref: "#/$defs/PackageCatalogEntry"
          },
          maxItems: 4096
        }
      },
      required: [
        "schemaVersion",
        "revision",
        "packages"
      ],
      additionalProperties: false
    },
    PackageCatalogSnapshot: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        objectRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        catalog: {
          $ref: "#/$defs/PackageCatalog"
        }
      },
      required: [
        "objectId",
        "objectRevision",
        "catalog"
      ],
      additionalProperties: false
    },
    PackageUploadResult: {
      oneOf: [
        {
          type: "object",
          properties: {
            alreadyPublished: {
              const: true
            },
            entry: {
              $ref: "#/$defs/PackageCatalogEntry"
            }
          },
          required: [
            "alreadyPublished",
            "entry"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            alreadyPublished: {
              const: false
            },
            uploadId: {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            uploadToken: {
              type: "string",
              minLength: 1,
              maxLength: 512
            },
            uploadPath: {
              type: "string",
              minLength: 1,
              maxLength: 2048
            },
            expiresAt: {
              type: "string",
              pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
            }
          },
          required: [
            "alreadyPublished",
            "uploadId",
            "uploadToken",
            "uploadPath",
            "expiresAt"
          ],
          additionalProperties: false
        }
      ]
    },
    Status: {
      type: "object",
      properties: {
        callerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        version: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        buildId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        hiveProtocol: {
          const: 1
        },
        runtimeEpoch: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Current Hive epoch for mutation IDs and services that explicitly require epoch-formatted operation IDs, including AgentManager and Secretary. Read again after a Hive restart; other services may use different ID formats."
        },
        ready: {
          type: "boolean"
        },
        serverTime: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        },
        storageFormat: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        publicBaseUrl: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        }
      },
      required: [
        "callerPrincipalId",
        "version",
        "buildId",
        "hiveProtocol",
        "runtimeEpoch",
        "ready",
        "serverTime",
        "storageFormat",
        "publicBaseUrl"
      ],
      additionalProperties: false
    },
    SystemStatusParams: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    SystemStatusResult: {
      $ref: "#/$defs/Status"
    },
    SystemInstructionsParams: {
      type: "object",
      properties: {
        view: {
          enum: [
            "instructions",
            "examples"
          ],
          description: "instructions (default) reads the exact current handshake text; examples reads tested public tool workflows."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 80,
          description: "Opaque continuation from the previous page. Omit to start at the beginning of the selected view."
        }
      },
      required: [],
      additionalProperties: false
    },
    SystemInstructionsResult: {
      type: "object",
      properties: {
        view: {
          enum: [
            "instructions",
            "examples"
          ]
        },
        text: {
          type: "string",
          maxLength: 32768
        },
        sha256: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        totalBytes: {
          type: "integer",
          minimum: 1,
          maximum: 32768
        },
        startByte: {
          type: "integer",
          minimum: 0,
          maximum: 32768
        },
        endByte: {
          type: "integer",
          minimum: 0,
          maximum: 32768
        },
        complete: {
          type: "boolean",
          description: "True only when this response contains the entire selected text."
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 80
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "view",
        "text",
        "sha256",
        "totalBytes",
        "startByte",
        "endByte",
        "complete",
        "nextCursor"
      ],
      additionalProperties: false
    },
    SystemToolSchemaParams: {
      type: "object",
      properties: {
        name: {
          type: "string",
          minLength: 1,
          maxLength: 128,
          description: "Exact public MCP tool name from tools/list, such as wiki_read."
        }
      },
      required: [
        "name"
      ],
      additionalProperties: false
    },
    SystemToolSchemaResult: {
      type: "object",
      properties: {
        name: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        description: {
          type: "string",
          maxLength: 65536
        },
        inputSchema: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        outputSchema: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
            },
            {
              type: "null"
            }
          ]
        },
        schemaHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        complete: {
          const: true
        }
      },
      required: [
        "name",
        "description",
        "inputSchema",
        "outputSchema",
        "schemaHash",
        "complete"
      ],
      additionalProperties: false
    },
    SystemInspectStorageParams: {
      type: "object",
      properties: {
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
              readScope: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractReadScope"
              }
            },
            required: [
              "key"
            ],
            additionalProperties: false
          },
          maxItems: 128
        }
      },
      required: [
        "contracts"
      ],
      additionalProperties: false
    },
    SystemInspectStorageResult: {
      $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/StorageInspection"
    },
    SystemDiagnosticsParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        },
        status: {
          enum: [
            "current",
            "stale",
            "unknown",
            "resolved",
            "unresolved"
          ],
          description: "Observation state to list; unresolved selects every state except resolved."
        }
      },
      required: [],
      additionalProperties: false
    },
    SystemDiagnosticsResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Diagnostic"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    ContractsListParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        },
        key: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        allVersions: {
          type: "boolean"
        }
      },
      required: [],
      additionalProperties: false
    },
    ContractsListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DataContract"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    ContractsSummariesParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        },
        key: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        allVersions: {
          type: "boolean"
        }
      },
      required: [],
      additionalProperties: false
    },
    ContractsSummariesResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/ContractSummary"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    ContractsGetParams: {
      type: "object",
      properties: {
        key: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        version: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        }
      },
      required: [
        "key"
      ],
      additionalProperties: false
    },
    ContractsGetResult: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DataContract"
    },
    ContractsRegisterParams: {
      type: "object",
      properties: {
        definition: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DataContract"
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "definition",
        "mutationId"
      ],
      additionalProperties: false
    },
    ContractsRegisterResult: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DataContract"
    },
    ContractsValidateParams: {
      type: "object",
      properties: {
        key: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        contractVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        content: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Content"
        }
      },
      required: [
        "key",
        "contractVersion",
        "content"
      ],
      additionalProperties: false
    },
    ContractsValidateResult: {
      type: "object",
      properties: {
        valid: {
          const: true
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        },
        byteLength: {
          type: "integer",
          minimum: 0,
          maximum: 8388608
        }
      },
      required: [
        "valid",
        "contentHash",
        "byteLength"
      ],
      additionalProperties: false
    },
    ObjectsStatParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable Hive Object ID returned by Object operations."
        },
        path: {
          type: "string",
          minLength: 1,
          maxLength: 16384,
          description: "Absolute Hive Object path; supply either path or objectId, not both."
        }
      },
      required: [],
      additionalProperties: false,
      oneOf: [
        {
          required: [
            "objectId"
          ],
          not: {
            required: [
              "path"
            ]
          }
        },
        {
          required: [
            "path"
          ],
          not: {
            required: [
              "objectId"
            ]
          }
        }
      ]
    },
    ObjectsStatResult: {
      $ref: "#/$defs/ObjectMetadata"
    },
    ObjectsReadParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable Hive Object ID returned by Object operations."
        },
        path: {
          type: "string",
          minLength: 1,
          maxLength: 16384,
          description: "Absolute Hive Object path; supply either path or objectId, not both."
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [],
      additionalProperties: false,
      oneOf: [
        {
          required: [
            "objectId"
          ],
          not: {
            required: [
              "path"
            ]
          }
        },
        {
          required: [
            "path"
          ],
          not: {
            required: [
              "objectId"
            ]
          }
        }
      ]
    },
    ObjectsReadResult: {
      $ref: "#/$defs/ObjectRead"
    },
    ObjectsWriteParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ObjectWrite"
    },
    ObjectsWriteResult: {
      $ref: "#/$defs/ObjectWriteResult"
    },
    ObjectsWriteReceiptParams: {
      type: "object",
      properties: {
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRequestHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "mutationId",
        "expectedRequestHash"
      ],
      additionalProperties: false
    },
    ObjectsWriteReceiptResult: {
      anyOf: [
        {
          $ref: "#/$defs/ObjectWriteResult"
        },
        {
          type: "null"
        }
      ]
    },
    ObjectsHistoryParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "objectId"
      ],
      additionalProperties: false
    },
    ObjectsHistoryResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/RevisionMetadata"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        historyComplete: {
          type: "boolean"
        }
      },
      required: [
        "items",
        "nextCursor",
        "historyComplete"
      ],
      additionalProperties: false
    },
    ObjectsPruneRevisionsParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        },
        maximumCount: {
          type: "integer",
          minimum: 1,
          maximum: 1e6
        },
        maximumAgeDays: {
          type: "number",
          exclusiveMinimum: 0
        },
        maximumBytes: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "objectId",
        "expectedRevision",
        "mutationId",
        "maximumCount",
        "maximumAgeDays",
        "maximumBytes"
      ],
      additionalProperties: false
    },
    ObjectsPruneRevisionsResult: {
      type: "object",
      properties: {
        deleted: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        protected: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        remainingBytes: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "deleted",
        "protected",
        "remainingBytes"
      ],
      additionalProperties: false
    },
    ObjectsListParams: {
      type: "object",
      properties: {
        parentId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        includeArchived: {
          type: "boolean"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "parentId"
      ],
      additionalProperties: false
    },
    ObjectsListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/ObjectMetadata"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    ObjectsTreeParams: {
      type: "object",
      properties: {
        rootId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        depth: {
          type: "integer",
          minimum: 1,
          maximum: 8
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 500
        },
        includeArchived: {
          type: "boolean"
        }
      },
      required: [
        "rootId"
      ],
      additionalProperties: false
    },
    ObjectsTreeResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/ObjectMetadata"
          },
          maxItems: 500
        },
        truncated: {
          type: "boolean"
        }
      },
      required: [
        "items",
        "truncated"
      ],
      additionalProperties: false
    },
    ObjectsQueryParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ObjectQuery"
    },
    ObjectsQueryResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/QueryItem"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    ObjectsSearchParams: {
      type: "object",
      properties: {
        text: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        contractKey: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        rootId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        includeArchived: {
          type: "boolean"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "text"
      ],
      additionalProperties: false
    },
    ObjectsSearchResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/ObjectMetadata"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    RetentionPreviewParams: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    RetentionPreviewResult: {
      $ref: "#/$defs/RetentionStatus"
    },
    RetentionStatusParams: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    RetentionStatusResult: {
      $ref: "#/$defs/RetentionStatus"
    },
    ObjectsMoveParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        parentId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        name: {
          type: "string",
          minLength: 1,
          maxLength: 255
        },
        icon: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 32
            },
            {
              type: "null"
            }
          ]
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "objectId",
        "parentId",
        "name",
        "mutationId"
      ],
      additionalProperties: false
    },
    ObjectsMoveResult: {
      $ref: "#/$defs/ObjectMetadata"
    },
    ObjectsReorderParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        beforeObjectId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "objectId",
        "beforeObjectId",
        "mutationId"
      ],
      additionalProperties: false
    },
    ObjectsReorderResult: {
      $ref: "#/$defs/ObjectMetadata"
    },
    ObjectsArchiveParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        archived: {
          type: "boolean"
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "objectId",
        "archived",
        "mutationId"
      ],
      additionalProperties: false
    },
    ObjectsArchiveResult: {
      $ref: "#/$defs/ObjectMetadata"
    },
    ObjectsDeleteParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "objectId",
        "expectedRevision",
        "mutationId"
      ],
      additionalProperties: false
    },
    ObjectsDeleteResult: {
      type: "object",
      properties: {
        deleted: {
          type: "boolean"
        }
      },
      required: [
        "deleted"
      ],
      additionalProperties: false
    },
    ServiceConnectParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ServiceConnect"
    },
    ServiceConnectResult: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        generation: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "serviceNodeId",
        "generation"
      ],
      additionalProperties: false
    },
    RegistrySyncParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/RegistrySync"
    },
    RegistrySyncResult: {
      type: "object",
      properties: {
        generation: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        syncedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        }
      },
      required: [
        "generation",
        "syncedAt"
      ],
      additionalProperties: false
    },
    ServiceHeartbeatParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ServiceHeartbeat"
    },
    ServiceHeartbeatResult: {
      type: "object",
      properties: {
        generation: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        ready: {
          type: "boolean"
        },
        observedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        }
      },
      required: [
        "generation",
        "ready",
        "observedAt"
      ],
      additionalProperties: false
    },
    HostsListParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [],
      additionalProperties: false
    },
    HostsListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/Host"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    HostsReportParams: {
      $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/ManagementSnapshot"
    },
    HostsReportResult: {
      type: "object",
      properties: {
        sequence: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        reportedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        }
      },
      required: [
        "sequence",
        "reportedAt"
      ],
      additionalProperties: false
    },
    HostsObservationsParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [],
      additionalProperties: false
    },
    HostsObservationsResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/ManagementObservation"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    HostConfigurationsListParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [],
      additionalProperties: false
    },
    HostConfigurationsListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/HostConfigurationSummary"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    HostConfigurationsGetParams: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "hostId"
      ],
      additionalProperties: false
    },
    HostConfigurationsGetResult: {
      $ref: "#/$defs/HostConfiguration"
    },
    HostConfigurationsPutParams: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        configuration: {
          $ref: "https://ivy.invalid/schemas/host.schema.json#/$defs/HostConfig"
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "hostId",
        "configuration",
        "mutationId"
      ],
      additionalProperties: false
    },
    HostConfigurationsPutResult: {
      $ref: "#/$defs/HostConfiguration"
    },
    HostConfigurationsEditParams: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "hostId"
      ],
      additionalProperties: false
    },
    HostConfigurationsEditResult: {
      $ref: "#/$defs/HostConfigurationEditor"
    },
    HostConfigurationsSaveParams: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        configuration: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "hostId",
        "configuration",
        "mutationId"
      ],
      additionalProperties: false
    },
    HostConfigurationsSaveResult: {
      $ref: "#/$defs/HostConfigurationEditor"
    },
    HostConfigurationsHistoryParams: {
      type: "object",
      properties: {
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "hostId"
      ],
      additionalProperties: false
    },
    HostConfigurationsHistoryResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/RevisionMetadata"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        },
        historyComplete: {
          type: "boolean"
        }
      },
      required: [
        "items",
        "nextCursor",
        "historyComplete"
      ],
      additionalProperties: false
    },
    ServiceNodesListParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64
        }
      },
      required: [],
      additionalProperties: false
    },
    ServiceNodesListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/ServiceNode"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    ServiceNodesGetParams: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "serviceNodeId"
      ],
      additionalProperties: false
    },
    ServiceNodesGetResult: {
      $ref: "#/$defs/ServiceNode"
    },
    ServiceNodesContractsParams: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        key: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "serviceNodeId"
      ],
      additionalProperties: false
    },
    ServiceNodesContractsResult: {
      $ref: "#/$defs/NodeContracts"
    },
    NamespacesListParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [],
      additionalProperties: false
    },
    NamespacesListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/NamespaceSummary"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    NamespacesGetParams: {
      type: "object",
      properties: {
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Exact registered provider node to select when a service has multiple providers."
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Provider host selector; it must agree with any service node or resource selector."
        },
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          description: "Registered service selector."
        },
        resourceRef: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef",
          description: "Resource owner selector obtained from an earlier Hive result."
        }
      },
      required: [
        "namespace"
      ],
      additionalProperties: false
    },
    NamespacesGetResult: {
      $ref: "#/$defs/NamespaceSnapshot"
    },
    ToolsListParams: {
      type: "object",
      properties: {
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Exact registered provider node to select when a service has multiple providers."
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Provider host selector; it must agree with any service node or resource selector."
        },
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          description: "Registered service selector."
        },
        resourceRef: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef",
          description: "Resource owner selector obtained from an earlier Hive result."
        },
        namePrefix: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "namespace"
      ],
      additionalProperties: false
    },
    ToolsListResult: {
      type: "object",
      properties: {
        provider: {
          $ref: "#/$defs/Provider"
        },
        guideMarkdown: {
          type: "string",
          maxLength: 65536
        },
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/ToolBinding"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "provider",
        "guideMarkdown",
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    TopicsListParams: {
      type: "object",
      properties: {
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Exact registered provider node to select when a service has multiple providers."
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Provider host selector; it must agree with any service node or resource selector."
        },
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          description: "Registered service selector."
        },
        resourceRef: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef",
          description: "Resource owner selector obtained from an earlier Hive result."
        },
        topicPrefix: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [],
      additionalProperties: false
    },
    TopicsListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            type: "object",
            properties: {
              provider: {
                anyOf: [
                  {
                    $ref: "#/$defs/Provider"
                  },
                  {
                    type: "null"
                  }
                ]
              },
              namespace: {
                type: "string",
                minLength: 1,
                maxLength: 64
              },
              definition: {
                $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/TopicDefinition"
              }
            },
            required: [
              "provider",
              "namespace",
              "definition"
            ],
            additionalProperties: false
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    ToolsCallParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ToolCall"
    },
    ToolsCallResult: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
    },
    DiscoveryListParams: {
      type: "object",
      properties: {
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          description: "Registered service to browse; a service-specific MCP tool supplies this selector."
        },
        group: {
          type: "string",
          minLength: 1,
          maxLength: 256,
          description: "Discovery group returned by an earlier browse result."
        },
        query: {
          type: "string",
          minLength: 1,
          maxLength: 512,
          description: "Capability keywords; all terms must match tool names, descriptions or keywords. Searches tools, not stored content."
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Exact provider node selected from a provider discovery result."
        },
        providers: {
          type: "boolean",
          description: "With serviceName, list hosts that provide it. Omit query, group and serviceNodeId."
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 20,
          description: "Maximum entries to return, from 1 through 20."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding discovery page; omit for the first page and keep selectors unchanged."
        }
      },
      required: [],
      additionalProperties: false
    },
    DiscoveryListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/DiscoveryEntry"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    DiscoveryInstructionsParams: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    DiscoveryInstructionsResult: {
      type: "object",
      properties: {
        instructions: {
          type: "string",
          minLength: 1,
          maxLength: 32768
        }
      },
      required: [
        "instructions"
      ],
      additionalProperties: false
    },
    DiscoveryDescribeParams: {
      type: "object",
      properties: {
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          description: "Registered service from discovery."
        },
        tools: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 256
          },
          maxItems: 5,
          minItems: 1,
          uniqueItems: true,
          description: "Exact qualified tool names returned by discovery, at most five."
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Exact provider node required when the service is ambiguous."
        }
      },
      required: [
        "serviceName",
        "tools"
      ],
      additionalProperties: false
    },
    DiscoveryDescribeResult: {
      type: "object",
      properties: {
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        provider: {
          anyOf: [
            {
              $ref: "#/$defs/DiscoveryProvider"
            },
            {
              type: "null"
            }
          ]
        },
        guides: {
          type: "array",
          items: {
            type: "object",
            properties: {
              namespace: {
                type: "string",
                minLength: 1,
                maxLength: 64
              },
              guideMarkdown: {
                type: "string",
                maxLength: 65536
              }
            },
            required: [
              "namespace",
              "guideMarkdown"
            ],
            additionalProperties: false
          },
          maxItems: 5
        },
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/ToolBinding"
          },
          maxItems: 5
        }
      },
      required: [
        "serviceName",
        "provider",
        "guides",
        "items"
      ],
      additionalProperties: false
    },
    DiscoveryCallParams: {
      type: "object",
      properties: {
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          description: "Registered service from the matching describe result."
        },
        qualifiedName: {
          type: "string",
          minLength: 1,
          maxLength: 256,
          description: "Exact qualified tool name from the describe result."
        },
        expectedDefinitionHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash",
          description: "Definition hash from the same describe result; invocation fails if the definition changed."
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Exact provider node from the describe result when present."
        },
        arguments: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json",
          description: "Arguments validated against the selected input schema returned by describe."
        },
        operationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable caller-generated identity for routed work; retain it to reconcile an unknown outcome."
        },
        expectedCallerPrincipalId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Optional caller identity fence obtained from authenticated status when required."
        }
      },
      required: [
        "serviceName",
        "qualifiedName",
        "expectedDefinitionHash",
        "arguments"
      ],
      additionalProperties: false
    },
    DiscoveryCallResult: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
    },
    InventoryListParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Exact registered provider node to select when a service has multiple providers."
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Provider host selector; it must agree with any service node or resource selector."
        },
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64,
          description: "Registered service selector."
        },
        resourceRef: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef",
          description: "Resource owner selector obtained from an earlier Hive result."
        },
        namespace: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        kind: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        sort: {
          enum: [
            "native-id",
            "recency-desc"
          ]
        },
        archived: {
          type: "boolean"
        },
        searchTerm: {
          type: "string",
          minLength: 1,
          maxLength: 512
        }
      },
      required: [],
      additionalProperties: false
    },
    InventoryListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/InventoryItem"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    InventoryGetParams: {
      type: "object",
      properties: {
        resourceRef: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ResourceRef"
        }
      },
      required: [
        "resourceRef"
      ],
      additionalProperties: false
    },
    InventoryGetResult: {
      $ref: "#/$defs/InventoryItem"
    },
    InventorySyncParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/InventorySync"
    },
    InventorySyncResult: {
      type: "object",
      properties: {
        snapshotRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        observedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        }
      },
      required: [
        "snapshotRevision",
        "observedAt"
      ],
      additionalProperties: false
    },
    EventsPublishParams: {
      type: "object",
      properties: {
        topic: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        topicVersion: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/ContractVersion"
        },
        payload: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Json"
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "topic",
        "topicVersion",
        "payload",
        "mutationId"
      ],
      additionalProperties: false
    },
    EventsPublishResult: {
      $ref: "#/$defs/EventPublication"
    },
    EventsReadParams: {
      type: "object",
      properties: {
        afterSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        filter: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/EventFilter"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 100
        }
      },
      required: [
        "afterSequence",
        "filter"
      ],
      additionalProperties: false
    },
    EventsReadResult: {
      $ref: "#/$defs/EventBatch"
    },
    EventsHeadParams: {
      type: "object",
      properties: {},
      required: [],
      additionalProperties: false
    },
    EventsHeadResult: {
      type: "object",
      properties: {
        throughSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "throughSequence"
      ],
      additionalProperties: false
    },
    EventsSubscribeParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/EventSubscribe"
    },
    EventsSubscribeResult: {
      $ref: "#/$defs/EventBatch"
    },
    EventsAckParams: {
      $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/EventAck"
    },
    EventsAckResult: {
      type: "object",
      properties: {
        acknowledgedSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "acknowledgedSequence"
      ],
      additionalProperties: false
    },
    EventsUnsubscribeParams: {
      type: "object",
      properties: {
        name: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "name",
        "mutationId"
      ],
      additionalProperties: false
    },
    EventsUnsubscribeResult: {
      type: "object",
      properties: {
        removed: {
          type: "boolean"
        }
      },
      required: [
        "removed"
      ],
      additionalProperties: false
    },
    NotificationsSubscribeParams: {
      type: "object",
      properties: {
        filters: {
          type: "array",
          items: {
            $ref: "#/$defs/NotificationFilter"
          },
          maxItems: 64
        },
        changes: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 512,
            pattern: "^(objects(?:/[a-zA-Z0-9._/-]+)?|services|inventory|uis|system)$"
          },
          maxItems: 64,
          uniqueItems: true,
          description: "Transient UI invalidations: objects or objects/<contract prefix>, services, inventory, uis and system. Reconnect rereads current state."
        }
      },
      required: [
        "filters"
      ],
      additionalProperties: false
    },
    NotificationsSubscribeResult: {
      type: "object",
      properties: {
        subscribed: {
          type: "integer",
          minimum: 0,
          maximum: 64
        },
        changes: {
          type: "integer",
          minimum: 0,
          maximum: 64
        }
      },
      required: [
        "subscribed"
      ],
      additionalProperties: false
    },
    PackagesCatalogParams: {
      type: "object",
      properties: {
        after: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        includeUis: {
          type: "boolean"
        }
      },
      required: [
        "after",
        "includeUis"
      ],
      additionalProperties: false
    },
    PackagesCatalogResult: {
      $ref: "#/$defs/PackageCatalogSnapshot"
    },
    PackagesAuthorizeUploadParams: {
      $ref: "#/$defs/PackageUploadAuthorization"
    },
    PackagesAuthorizeUploadResult: {
      $ref: "#/$defs/PackageUploadResult"
    },
    UisListParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [],
      additionalProperties: false
    },
    UisListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/UI"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    UisGetParams: {
      type: "object",
      properties: {
        uiId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        }
      },
      required: [
        "uiId"
      ],
      additionalProperties: false
    },
    UisGetResult: {
      $ref: "#/$defs/UI"
    },
    UisCatalogParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [],
      additionalProperties: false
    },
    UisCatalogResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/UiSummary"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    UisReleasesParams: {
      type: "object",
      properties: {
        uiId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "uiId"
      ],
      additionalProperties: false
    },
    UisReleasesResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/UiReleaseSummary"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    UisInspectParams: {
      type: "object",
      properties: {
        uiId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        releaseId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        }
      },
      required: [
        "uiId"
      ],
      additionalProperties: false
    },
    UisInspectResult: {
      $ref: "#/$defs/UiInspection"
    },
    UisStageAssetParams: {
      type: "object",
      properties: {
        uiId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        releaseId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        asset: {
          $ref: "#/$defs/UiAsset"
        },
        base64: {
          type: "string",
          maxLength: 11184812
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "uiId",
        "releaseId",
        "asset",
        "base64",
        "mutationId"
      ],
      additionalProperties: false
    },
    UisStageAssetResult: {
      type: "object",
      properties: {
        staged: {
          type: "boolean"
        }
      },
      required: [
        "staged"
      ],
      additionalProperties: false
    },
    UisDeployParams: {
      type: "object",
      properties: {
        metadata: {
          $ref: "#/$defs/UiMetadata"
        },
        release: {
          $ref: "#/$defs/UiRelease"
        },
        expectedReleaseId: {
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
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "metadata",
        "release",
        "expectedReleaseId",
        "mutationId"
      ],
      additionalProperties: false
    },
    UisDeployResult: {
      $ref: "#/$defs/UiPointerResult"
    },
    UisRollbackParams: {
      type: "object",
      properties: {
        uiId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        releaseId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        expectedReleaseId: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "uiId",
        "releaseId",
        "expectedReleaseId",
        "mutationId"
      ],
      additionalProperties: false
    },
    UisRollbackResult: {
      $ref: "#/$defs/UiPointerResult"
    },
    DeploymentsListParams: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        instanceId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [],
      additionalProperties: false
    },
    DeploymentsListResult: {
      type: "object",
      properties: {
        items: {
          type: "array",
          items: {
            $ref: "#/$defs/DeploymentSnapshot"
          },
          maxItems: 200
        },
        nextCursor: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 8192
            },
            {
              type: "null"
            }
          ]
        }
      },
      required: [
        "items",
        "nextCursor"
      ],
      additionalProperties: false
    },
    DeploymentsGetParams: {
      type: "object",
      properties: {
        deploymentId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        hostId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        }
      },
      required: [
        "deploymentId",
        "hostId"
      ],
      additionalProperties: false
    },
    DeploymentsGetResult: {
      $ref: "#/$defs/DeploymentSnapshot"
    },
    DeploymentsReportParams: {
      type: "object",
      properties: {
        records: {
          type: "array",
          items: {
            $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/DeploymentRecord"
          },
          maxItems: 200
        }
      },
      required: [
        "records"
      ],
      additionalProperties: false
    },
    DeploymentsReportResult: {
      type: "object",
      properties: {
        reportedAt: {
          type: "string",
          pattern: "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$"
        }
      },
      required: [
        "reportedAt"
      ],
      additionalProperties: false
    },
    WikiSearchParams: {
      type: "object",
      properties: {
        text: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        rootId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        includeArchived: {
          type: "boolean"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "text"
      ],
      additionalProperties: false
    },
    WikiSearchResult: {
      $ref: "#/$defs/ObjectsSearchResult"
    },
    WikiListParams: {
      type: "object",
      properties: {
        parentId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        },
        includeArchived: {
          type: "boolean"
        }
      },
      required: [],
      additionalProperties: false
    },
    WikiListResult: {
      $ref: "#/$defs/ObjectsQueryResult"
    },
    WikiReadParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable Hive Object ID returned by Object operations."
        },
        path: {
          type: "string",
          minLength: 1,
          maxLength: 16384,
          description: "Absolute Hive Object path; supply either path or objectId, not both."
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [],
      additionalProperties: false,
      oneOf: [
        {
          required: [
            "objectId"
          ],
          not: {
            required: [
              "path"
            ]
          }
        },
        {
          required: [
            "path"
          ],
          not: {
            required: [
              "objectId"
            ]
          }
        }
      ]
    },
    WikiReadResult: {
      $ref: "#/$defs/ObjectRead"
    },
    WikiCreateParams: {
      type: "object",
      properties: {
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        },
        title: {
          type: "string",
          minLength: 1,
          maxLength: 255
        },
        parentId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        markdown: {
          type: "string",
          maxLength: 1048576
        }
      },
      required: [
        "mutationId",
        "title",
        "markdown"
      ],
      additionalProperties: false
    },
    WikiCreateResult: {
      $ref: "#/$defs/ObjectWriteResult"
    },
    WikiUpdateParams: {
      type: "object",
      properties: {
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        },
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        markdown: {
          type: "string",
          maxLength: 1048576
        }
      },
      required: [
        "mutationId",
        "objectId",
        "expectedRevision",
        "markdown"
      ],
      additionalProperties: false
    },
    WikiUpdateResult: {
      $ref: "#/$defs/ObjectWriteResult"
    },
    WikiHistoryParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200,
          description: "Maximum items to return on this page."
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192,
          description: "Opaque cursor from the preceding page; omit for the first page and keep all other selectors unchanged."
        }
      },
      required: [
        "objectId"
      ],
      additionalProperties: false
    },
    WikiHistoryResult: {
      $ref: "#/$defs/ObjectsHistoryResult"
    },
    WikiMoveParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        parentId: {
          anyOf: [
            {
              $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
            },
            {
              type: "null"
            }
          ]
        },
        name: {
          type: "string",
          minLength: 1,
          maxLength: 255
        },
        icon: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 32
            },
            {
              type: "null"
            }
          ]
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        }
      },
      required: [
        "objectId",
        "parentId",
        "name",
        "mutationId"
      ],
      additionalProperties: false
    },
    WikiMoveResult: {
      $ref: "#/$defs/ObjectsMoveResult"
    },
    WikiArchiveParams: {
      type: "object",
      properties: {
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        archived: {
          type: "boolean"
        },
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable mutation identity: <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. The timestamp may be at most 60 seconds ahead and expires after 24 hours. A Hive restart changes the epoch. Retain this exact ID and inspect the original outcome before retrying an uncertain action."
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "objectId",
        "archived",
        "mutationId"
      ],
      additionalProperties: false
    },
    WikiArchiveResult: {
      $ref: "#/$defs/ObjectsArchiveResult"
    }
  }
};

// specs/schemas/hive-transport.schema.json
var hive_transport_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/hive-transport.schema.json",
  title: "Hive provider forwarding and transient notification frames",
  $defs: {
    ProviderCall: {
      type: "object",
      properties: {
        jsonrpc: {
          const: "2.0"
        },
        id: {
          $ref: "hive-wire.schema.json#/$defs/Identifier"
        },
        method: {
          const: "provider.invoke"
        },
        params: {
          type: "object",
          properties: {
            qualifiedName: {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            arguments: {
              $ref: "hive-wire.schema.json#/$defs/Json"
            },
            definitionHash: {
              $ref: "hive-wire.schema.json#/$defs/Hash"
            },
            generation: {
              type: "integer",
              minimum: 1,
              maximum: 9007199254740991
            },
            callerPrincipalId: {
              $ref: "hive-wire.schema.json#/$defs/Identifier"
            },
            operationId: {
              $ref: "hive-wire.schema.json#/$defs/Identifier"
            }
          },
          required: [
            "qualifiedName",
            "arguments",
            "definitionHash",
            "generation",
            "callerPrincipalId"
          ],
          additionalProperties: false
        }
      },
      required: [
        "jsonrpc",
        "id",
        "method",
        "params"
      ],
      additionalProperties: false
    },
    ServiceNotification: {
      type: "object",
      properties: {
        jsonrpc: {
          const: "2.0"
        },
        method: {
          const: "service.notification"
        },
        params: {
          type: "object",
          properties: {
            namespace: {
              type: "string",
              pattern: "^[a-z][a-z0-9-]*$"
            },
            name: {
              type: "string",
              minLength: 1,
              maxLength: 192
            },
            version: {
              $ref: "hive-wire.schema.json#/$defs/ContractVersion"
            },
            payload: {
              $ref: "hive-wire.schema.json#/$defs/Json"
            }
          },
          required: [
            "namespace",
            "name",
            "version",
            "payload"
          ],
          additionalProperties: false
        }
      },
      required: [
        "jsonrpc",
        "method",
        "params"
      ],
      additionalProperties: false
    },
    ProviderNotification: {
      type: "object",
      properties: {
        jsonrpc: { const: "2.0" },
        method: { const: "notifications.provider" },
        params: {
          type: "object",
          properties: {
            namespace: { type: "string", pattern: "^[a-z][a-z0-9-]*$" },
            name: { type: "string", minLength: 1, maxLength: 192 },
            version: { $ref: "hive-wire.schema.json#/$defs/ContractVersion" },
            payload: { $ref: "hive-wire.schema.json#/$defs/Json" },
            serviceNodeId: { $ref: "hive-wire.schema.json#/$defs/Identifier" },
            generation: { type: "integer", minimum: 1, maximum: 9007199254740991 }
          },
          required: ["namespace", "name", "version", "payload", "serviceNodeId", "generation"],
          additionalProperties: false
        }
      },
      required: ["jsonrpc", "method", "params"],
      additionalProperties: false
    },
    ChangeNotification: {
      type: "object",
      properties: {
        jsonrpc: { const: "2.0" },
        method: { const: "notifications.changed" },
        params: {
          type: "object",
          properties: {
            scopes: {
              description: "An empty list is a liveness heartbeat; nonempty scopes invalidate matching UI reads.",
              type: "array",
              maxItems: 64,
              uniqueItems: true,
              items: { type: "string", maxLength: 512, pattern: "^(objects(?:/[a-zA-Z0-9._/-]+)?|services|inventory|uis|system)$" }
            }
          },
          required: ["scopes"],
          additionalProperties: false
        }
      },
      required: ["jsonrpc", "method", "params"],
      additionalProperties: false
    },
    EventAvailability: {
      type: "object",
      properties: {
        jsonrpc: { const: "2.0" },
        method: { const: "events.available" },
        params: {
          type: "object",
          properties: {
            throughSequence: { type: "integer", minimum: 1, maximum: 9007199254740991 }
          },
          required: ["throughSequence"],
          additionalProperties: false
        }
      },
      required: ["jsonrpc", "method", "params"],
      additionalProperties: false
    }
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
            "0.154.0",
            "0.158.0"
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
    StorageRetentionStatus: {
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
        state: {
          enum: [
            "running",
            "succeeded",
            "partial",
            "failed"
          ]
        },
        trigger: {
          enum: [
            "manual",
            "scheduled",
            "storage_pressure"
          ]
        },
        startedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        completedAt: {
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
        lastSuccessAt: {
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
        removed: {
          type: "object",
          properties: {
            candidates: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            preparations: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            payloads: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            snapshots: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            maintenance: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            bootstrapPlans: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            }
          },
          required: [
            "candidates",
            "preparations",
            "payloads",
            "snapshots",
            "maintenance",
            "bootstrapPlans"
          ],
          additionalProperties: false
        },
        availableBytes: {
          type: "object",
          properties: {
            runtime: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            artifacts: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            },
            staging: {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            }
          },
          required: [
            "runtime",
            "artifacts",
            "staging"
          ],
          additionalProperties: false
        },
        lowSpace: {
          type: "boolean"
        },
        skipped: {
          type: "array",
          items: {
            type: "object",
            properties: {
              area: {
                type: "string",
                minLength: 1,
                maxLength: 64
              },
              reason: {
                type: "string",
                minLength: 1,
                maxLength: 128
              },
              count: {
                type: "integer",
                minimum: 1,
                maximum: 9007199254740991
              },
              requiresAttention: {
                type: "boolean"
              }
            },
            required: [
              "area",
              "reason",
              "count",
              "requiresAttention"
            ],
            additionalProperties: false
          },
          maxItems: 64
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
        "hostId",
        "state",
        "trigger",
        "startedAt",
        "completedAt",
        "lastSuccessAt",
        "removed",
        "availableBytes",
        "lowSpace",
        "skipped",
        "errorCode"
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
        },
        storageRetention: {
          $ref: "#/$defs/StorageRetentionStatus"
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

// specs/schemas/operations.json
var operations_default = {
  schemaVersion: 1,
  operations: {
    "system.status": {
      input: "#/$defs/SystemStatusParams",
      output: "#/$defs/SystemStatusResult",
      access: "client",
      mutation: false
    },
    "system.instructions": {
      input: "#/$defs/SystemInstructionsParams",
      output: "#/$defs/SystemInstructionsResult",
      access: "client",
      mutation: false
    },
    "system.toolSchema": {
      input: "#/$defs/SystemToolSchemaParams",
      output: "#/$defs/SystemToolSchemaResult",
      access: "client",
      mutation: false
    },
    "system.inspectStorage": {
      input: "#/$defs/SystemInspectStorageParams",
      output: "#/$defs/SystemInspectStorageResult",
      access: "client",
      mutation: false
    },
    "system.diagnostics": {
      input: "#/$defs/SystemDiagnosticsParams",
      output: "#/$defs/SystemDiagnosticsResult",
      access: "client",
      mutation: false
    },
    "contracts.list": {
      input: "#/$defs/ContractsListParams",
      output: "#/$defs/ContractsListResult",
      access: "client",
      mutation: false,
      discoverable: false
    },
    "contracts.summaries": {
      input: "#/$defs/ContractsSummariesParams",
      output: "#/$defs/ContractsSummariesResult",
      access: "client",
      mutation: false
    },
    "contracts.get": {
      input: "#/$defs/ContractsGetParams",
      output: "#/$defs/ContractsGetResult",
      access: "client",
      mutation: false
    },
    "contracts.register": {
      input: "#/$defs/ContractsRegisterParams",
      output: "#/$defs/ContractsRegisterResult",
      access: "client",
      mutation: true
    },
    "contracts.validate": {
      input: "#/$defs/ContractsValidateParams",
      output: "#/$defs/ContractsValidateResult",
      access: "client",
      mutation: false
    },
    "objects.stat": {
      input: "#/$defs/ObjectsStatParams",
      output: "#/$defs/ObjectsStatResult",
      access: "client",
      mutation: false
    },
    "objects.read": {
      input: "#/$defs/ObjectsReadParams",
      output: "#/$defs/ObjectsReadResult",
      access: "client",
      mutation: false
    },
    "objects.write": {
      input: "#/$defs/ObjectsWriteParams",
      output: "#/$defs/ObjectsWriteResult",
      access: "client",
      mutation: true
    },
    "objects.writeReceipt": {
      input: "#/$defs/ObjectsWriteReceiptParams",
      output: "#/$defs/ObjectsWriteReceiptResult",
      access: "service",
      mutation: false,
      discoverable: false
    },
    "objects.history": {
      input: "#/$defs/ObjectsHistoryParams",
      output: "#/$defs/ObjectsHistoryResult",
      access: "client",
      mutation: false
    },
    "objects.pruneRevisions": {
      input: "#/$defs/ObjectsPruneRevisionsParams",
      output: "#/$defs/ObjectsPruneRevisionsResult",
      access: "service",
      mutation: true,
      discoverable: false
    },
    "objects.list": {
      input: "#/$defs/ObjectsListParams",
      output: "#/$defs/ObjectsListResult",
      access: "client",
      mutation: false
    },
    "objects.tree": {
      input: "#/$defs/ObjectsTreeParams",
      output: "#/$defs/ObjectsTreeResult",
      access: "client",
      mutation: false
    },
    "objects.query": {
      input: "#/$defs/ObjectsQueryParams",
      output: "#/$defs/ObjectsQueryResult",
      access: "client",
      mutation: false
    },
    "objects.search": {
      input: "#/$defs/ObjectsSearchParams",
      output: "#/$defs/ObjectsSearchResult",
      access: "client",
      mutation: false
    },
    "retention.preview": {
      input: "#/$defs/RetentionPreviewParams",
      output: "#/$defs/RetentionPreviewResult",
      access: "client",
      mutation: false
    },
    "retention.status": {
      input: "#/$defs/RetentionStatusParams",
      output: "#/$defs/RetentionStatusResult",
      access: "client",
      mutation: false
    },
    "objects.move": {
      input: "#/$defs/ObjectsMoveParams",
      output: "#/$defs/ObjectsMoveResult",
      access: "client",
      mutation: true
    },
    "objects.reorder": {
      input: "#/$defs/ObjectsReorderParams",
      output: "#/$defs/ObjectsReorderResult",
      access: "client",
      mutation: true
    },
    "objects.archive": {
      input: "#/$defs/ObjectsArchiveParams",
      output: "#/$defs/ObjectsArchiveResult",
      access: "client",
      mutation: true
    },
    "objects.delete": {
      input: "#/$defs/ObjectsDeleteParams",
      output: "#/$defs/ObjectsDeleteResult",
      access: "client",
      mutation: true,
      discoverable: false
    },
    "service.connect": {
      input: "#/$defs/ServiceConnectParams",
      output: "#/$defs/ServiceConnectResult",
      access: "service",
      mutation: false
    },
    "registry.sync": {
      input: "#/$defs/RegistrySyncParams",
      output: "#/$defs/RegistrySyncResult",
      access: "service",
      mutation: false
    },
    "service.heartbeat": {
      input: "#/$defs/ServiceHeartbeatParams",
      output: "#/$defs/ServiceHeartbeatResult",
      access: "service",
      mutation: false
    },
    "hosts.list": {
      input: "#/$defs/HostsListParams",
      output: "#/$defs/HostsListResult",
      access: "client",
      mutation: false
    },
    "hosts.report": {
      input: "#/$defs/HostsReportParams",
      output: "#/$defs/HostsReportResult",
      access: "service",
      mutation: false
    },
    "hosts.observations": {
      input: "#/$defs/HostsObservationsParams",
      output: "#/$defs/HostsObservationsResult",
      access: "client",
      mutation: false
    },
    "hostConfigurations.list": {
      input: "#/$defs/HostConfigurationsListParams",
      output: "#/$defs/HostConfigurationsListResult",
      access: "client",
      mutation: false
    },
    "hostConfigurations.get": {
      input: "#/$defs/HostConfigurationsGetParams",
      output: "#/$defs/HostConfigurationsGetResult",
      access: "client",
      mutation: false
    },
    "hostConfigurations.put": {
      input: "#/$defs/HostConfigurationsPutParams",
      output: "#/$defs/HostConfigurationsPutResult",
      access: "client",
      mutation: true
    },
    "hostConfigurations.edit": {
      input: "#/$defs/HostConfigurationsEditParams",
      output: "#/$defs/HostConfigurationsEditResult",
      access: "client",
      mutation: false
    },
    "hostConfigurations.save": {
      input: "#/$defs/HostConfigurationsSaveParams",
      output: "#/$defs/HostConfigurationsSaveResult",
      access: "client",
      mutation: true
    },
    "hostConfigurations.history": {
      input: "#/$defs/HostConfigurationsHistoryParams",
      output: "#/$defs/HostConfigurationsHistoryResult",
      access: "client",
      mutation: false
    },
    "serviceNodes.list": {
      input: "#/$defs/ServiceNodesListParams",
      output: "#/$defs/ServiceNodesListResult",
      access: "client",
      mutation: false
    },
    "serviceNodes.get": {
      input: "#/$defs/ServiceNodesGetParams",
      output: "#/$defs/ServiceNodesGetResult",
      access: "client",
      mutation: false
    },
    "serviceNodes.contracts": {
      input: "#/$defs/ServiceNodesContractsParams",
      output: "#/$defs/ServiceNodesContractsResult",
      access: "client",
      mutation: false
    },
    "namespaces.list": {
      input: "#/$defs/NamespacesListParams",
      output: "#/$defs/NamespacesListResult",
      access: "client",
      mutation: false
    },
    "namespaces.get": {
      input: "#/$defs/NamespacesGetParams",
      output: "#/$defs/NamespacesGetResult",
      access: "client",
      mutation: false
    },
    "tools.list": {
      input: "#/$defs/ToolsListParams",
      output: "#/$defs/ToolsListResult",
      access: "client",
      mutation: false
    },
    "topics.list": {
      input: "#/$defs/TopicsListParams",
      output: "#/$defs/TopicsListResult",
      access: "client",
      mutation: false
    },
    "tools.call": {
      input: "#/$defs/ToolsCallParams",
      output: "#/$defs/ToolsCallResult",
      access: "client",
      mutation: false
    },
    "discovery.list": {
      input: "#/$defs/DiscoveryListParams",
      output: "#/$defs/DiscoveryListResult",
      access: "client",
      mutation: false
    },
    "discovery.instructions": {
      input: "#/$defs/DiscoveryInstructionsParams",
      output: "#/$defs/DiscoveryInstructionsResult",
      access: "client",
      mutation: false,
      discoverable: false
    },
    "discovery.describe": {
      input: "#/$defs/DiscoveryDescribeParams",
      output: "#/$defs/DiscoveryDescribeResult",
      access: "client",
      mutation: false
    },
    "discovery.call": {
      input: "#/$defs/DiscoveryCallParams",
      output: "#/$defs/DiscoveryCallResult",
      access: "client",
      mutation: false
    },
    "inventory.list": {
      input: "#/$defs/InventoryListParams",
      output: "#/$defs/InventoryListResult",
      access: "client",
      mutation: false
    },
    "inventory.get": {
      input: "#/$defs/InventoryGetParams",
      output: "#/$defs/InventoryGetResult",
      access: "client",
      mutation: false
    },
    "inventory.sync": {
      input: "#/$defs/InventorySyncParams",
      output: "#/$defs/InventorySyncResult",
      access: "service",
      mutation: false
    },
    "events.publish": {
      input: "#/$defs/EventsPublishParams",
      output: "#/$defs/EventsPublishResult",
      access: "service",
      mutation: true
    },
    "events.read": {
      input: "#/$defs/EventsReadParams",
      output: "#/$defs/EventsReadResult",
      access: "client",
      mutation: false
    },
    "events.head": {
      input: "#/$defs/EventsHeadParams",
      output: "#/$defs/EventsHeadResult",
      access: "service",
      mutation: false
    },
    "events.subscribe": {
      input: "#/$defs/EventsSubscribeParams",
      output: "#/$defs/EventsSubscribeResult",
      access: "service",
      mutation: false
    },
    "events.ack": {
      input: "#/$defs/EventsAckParams",
      output: "#/$defs/EventsAckResult",
      access: "service",
      mutation: false
    },
    "events.unsubscribe": {
      input: "#/$defs/EventsUnsubscribeParams",
      output: "#/$defs/EventsUnsubscribeResult",
      access: "service",
      mutation: true
    },
    "notifications.subscribe": {
      input: "#/$defs/NotificationsSubscribeParams",
      output: "#/$defs/NotificationsSubscribeResult",
      access: "client",
      mutation: false,
      discoverable: false
    },
    "packages.catalog": {
      input: "#/$defs/PackagesCatalogParams",
      output: "#/$defs/PackagesCatalogResult",
      access: "client",
      mutation: false
    },
    "packages.authorizeUpload": {
      input: "#/$defs/PackagesAuthorizeUploadParams",
      output: "#/$defs/PackagesAuthorizeUploadResult",
      access: "client",
      mutation: true
    },
    "uis.list": {
      input: "#/$defs/UisListParams",
      output: "#/$defs/UisListResult",
      access: "client",
      mutation: false
    },
    "uis.get": {
      input: "#/$defs/UisGetParams",
      output: "#/$defs/UisGetResult",
      access: "client",
      mutation: false
    },
    "uis.catalog": {
      input: "#/$defs/UisCatalogParams",
      output: "#/$defs/UisCatalogResult",
      access: "client",
      mutation: false
    },
    "uis.releases": {
      input: "#/$defs/UisReleasesParams",
      output: "#/$defs/UisReleasesResult",
      access: "client",
      mutation: false
    },
    "uis.inspect": {
      input: "#/$defs/UisInspectParams",
      output: "#/$defs/UisInspectResult",
      access: "client",
      mutation: false
    },
    "uis.stageAsset": {
      input: "#/$defs/UisStageAssetParams",
      output: "#/$defs/UisStageAssetResult",
      access: "client",
      mutation: true,
      discoverable: false
    },
    "uis.deploy": {
      input: "#/$defs/UisDeployParams",
      output: "#/$defs/UisDeployResult",
      access: "client",
      mutation: true
    },
    "uis.rollback": {
      input: "#/$defs/UisRollbackParams",
      output: "#/$defs/UisRollbackResult",
      access: "client",
      mutation: true
    },
    "deployments.list": {
      input: "#/$defs/DeploymentsListParams",
      output: "#/$defs/DeploymentsListResult",
      access: "client",
      mutation: false
    },
    "deployments.get": {
      input: "#/$defs/DeploymentsGetParams",
      output: "#/$defs/DeploymentsGetResult",
      access: "client",
      mutation: false
    },
    "deployments.report": {
      input: "#/$defs/DeploymentsReportParams",
      output: "#/$defs/DeploymentsReportResult",
      access: "service",
      mutation: false
    },
    "wiki.search": {
      input: "#/$defs/WikiSearchParams",
      output: "#/$defs/WikiSearchResult",
      access: "client",
      mutation: false
    },
    "wiki.list": {
      input: "#/$defs/WikiListParams",
      output: "#/$defs/WikiListResult",
      access: "client",
      mutation: false
    },
    "wiki.read": {
      input: "#/$defs/WikiReadParams",
      output: "#/$defs/WikiReadResult",
      access: "client",
      mutation: false
    },
    "wiki.create": {
      input: "#/$defs/WikiCreateParams",
      output: "#/$defs/WikiCreateResult",
      access: "client",
      mutation: true
    },
    "wiki.update": {
      input: "#/$defs/WikiUpdateParams",
      output: "#/$defs/WikiUpdateResult",
      access: "client",
      mutation: true
    },
    "wiki.history": {
      input: "#/$defs/WikiHistoryParams",
      output: "#/$defs/WikiHistoryResult",
      access: "client",
      mutation: false
    },
    "wiki.move": {
      input: "#/$defs/WikiMoveParams",
      output: "#/$defs/WikiMoveResult",
      access: "client",
      mutation: true
    },
    "wiki.archive": {
      input: "#/$defs/WikiArchiveParams",
      output: "#/$defs/WikiArchiveResult",
      access: "client",
      mutation: true
    }
  }
};

// packages/contracts/src/core-validation.ts
import { Ajv2020 } from "ajv/dist/2020.js";
var operationSchema = hive_operations_schema_default;
var transportSchema = hive_transport_schema_default;
var operations = operations_default.operations;
var ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema2 of [wireSchema, operationSchema, transportSchema, host_schema_default]) ajv.addSchema(schema2);
var get = (ref) => {
  const validator = ajv.getSchema(ref);
  if (!validator) throw new Error(`Unknown internal schema: ${ref}`);
  return validator;
};
var inputValidators = /* @__PURE__ */ new Map();
var outputValidators = /* @__PURE__ */ new Map();
function operationValidator(method, output) {
  if (!Object.hasOwn(operations, method)) return void 0;
  const validators = output ? outputValidators : inputValidators;
  let validator = validators.get(method);
  if (!validator) {
    const definition = operations[method];
    validator = get(String(operationSchema["$id"]) + (output ? definition.output : definition.input));
    validators.set(method, validator);
  }
  return validator;
}
function validateInput(method, value) {
  const validator = operationValidator(method, false);
  requireThat(validator, "not_found", "Unknown Hive operation.");
  requireThat(validator(value), "invalid_arguments", "Arguments do not match the operation contract.");
}
function validateOutput(method, value) {
  const validator = operationValidator(method, true);
  requireThat(validator && validator(value), "internal_error", "Result does not match the operation contract.");
}
function validateShared(name, value) {
  requireThat(get(`${String(wireSchema["$id"])}#/$defs/${name}`)(value), "invalid_arguments", "Value does not match its shared contract.");
}
function validateTransport(name, value) {
  requireThat(get(`${String(transportSchema["$id"])}#/$defs/${name}`)(value), "invalid_frame", "Invalid provider transport frame.");
}

// packages/contracts/src/host-validation.ts
import { Ajv2020 as Ajv20202 } from "ajv/dist/2020.js";
var schema = host_schema_default;
var ajv2 = new Ajv20202({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
ajv2.addSchema(schema);
ajv2.addSchema(hive_wire_schema_default);
function validateHost(name, value) {
  canonical(value, 32 * 1024 * 1024);
  const validator = ajv2.getSchema(`${schema.$id}#/$defs/${name}`);
  requireThat(validator && validator(value), "invalid_arguments", "Value does not match its host configuration/operation contract.");
}
var hostSchema = schema;

export {
  hive_operations_schema_default,
  host_schema_default,
  operationSchema,
  validateInput,
  validateOutput,
  validateShared,
  validateTransport,
  validateHost,
  hostSchema
};
//# sourceMappingURL=chunk-BD7CXXG2.js.map
