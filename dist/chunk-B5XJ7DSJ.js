import {
  wireSchema
} from "./chunk-FPBO6QV7.js";
import {
  host_schema_default
} from "./chunk-MEXDZTG7.js";
import {
  requireThat
} from "./chunk-62CEFMQW.js";

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
        }
      },
      required: [
        "namespace",
        "description",
        "guideMarkdown",
        "provider",
        "toolCount"
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
            "settings"
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
        objectId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
        },
        revision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        contentHash: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Hash"
        }
      },
      required: [
        "path",
        "objectId",
        "revision",
        "mediaType",
        "contentHash"
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
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier"
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
            "resolved"
          ]
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
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
        mutationId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
          description: "Case-insensitive tool search text."
        },
        serviceNodeId: {
          $ref: "https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/Identifier",
          description: "Exact provider node selected from a provider discovery result."
        },
        providers: {
          type: "boolean",
          description: "List matching providers instead of capability groups or tools."
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
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
          description: "Stable caller-generated identity for this mutation; reuse it only to recover or replay the same intended effect."
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
    "objects.history": {
      input: "#/$defs/ObjectsHistoryParams",
      output: "#/$defs/ObjectsHistoryResult",
      access: "client",
      mutation: false
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
    "objects.archive": {
      input: "#/$defs/ObjectsArchiveParams",
      output: "#/$defs/ObjectsArchiveResult",
      access: "client",
      mutation: true
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
    }
  }
};

// packages/contracts/src/core-validation.ts
import { Ajv2020 } from "ajv/dist/2020.js";
var operationSchema = hive_operations_schema_default;
var transportSchema = hive_transport_schema_default;
var operations = operations_default.operations;
var ajv = new Ajv2020({ strict: false, allErrors: false, validateFormats: false, ownProperties: true });
for (const schema of [wireSchema, operationSchema, transportSchema, host_schema_default]) ajv.addSchema(schema);
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

export {
  hive_operations_schema_default,
  operationSchema,
  validateInput,
  validateOutput,
  validateShared,
  validateTransport
};
//# sourceMappingURL=chunk-B5XJ7DSJ.js.map
