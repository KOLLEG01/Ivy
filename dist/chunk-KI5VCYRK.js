import {
  canonical
} from "./chunk-BO4WKKA7.js";

// packages/contracts/src/canonical.ts
import { createHash } from "node:crypto";
var digest = (bytes) => "sha256:" + createHash("sha256").update(bytes).digest("hex");
var hashJson = (value) => digest(canonical(value));

// specs/schemas/hive-wire.schema.json
var hive_wire_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://ivy.invalid/schemas/hive-wire.schema.json",
  title: "Ivy protocol 1 shared wire definitions",
  description: "Normative shared shapes. State, media, ownership, hashing, range and operation-specific semantic checks remain required by the specification.",
  oneOf: [
    {
      $ref: "#/$defs/Request"
    },
    {
      $ref: "#/$defs/Success"
    },
    {
      $ref: "#/$defs/Failure"
    }
  ],
  $defs: {
    Json: {
      oneOf: [
        {
          type: "null"
        },
        {
          type: "boolean"
        },
        {
          type: "number"
        },
        {
          type: "string"
        },
        {
          type: "array",
          items: {
            $ref: "#/$defs/Json"
          }
        },
        {
          type: "object",
          additionalProperties: {
            $ref: "#/$defs/Json"
          }
        }
      ]
    },
    Identifier: {
      type: "string",
      minLength: 1,
      maxLength: 256
    },
    ContractVersion: {
      type: "string",
      pattern: "^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"
    },
    Hash: {
      type: "string",
      pattern: "^sha256:[0-9a-f]{64}$"
    },
    ResourceRef: {
      type: "object",
      properties: {
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 256
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
        nativeId: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        }
      },
      required: [
        "serviceNodeId",
        "namespace",
        "kind",
        "nativeId"
      ],
      additionalProperties: false
    },
    Error: {
      type: "object",
      properties: {
        code: {
          type: "integer"
        },
        message: {
          type: "string",
          minLength: 1,
          maxLength: 2048
        },
        data: {
          type: "object",
          properties: {
            code: {
              type: "string",
              minLength: 1,
              maxLength: 128
            },
            outcome: {
              enum: [
                "not_executed",
                "unknown",
                "completed"
              ]
            },
            details: {
              $ref: "#/$defs/Json"
            }
          },
          required: [
            "code",
            "outcome"
          ],
          additionalProperties: false
        }
      },
      required: [
        "code",
        "message",
        "data"
      ],
      additionalProperties: false
    },
    Request: {
      type: "object",
      properties: {
        jsonrpc: {
          const: "2.0"
        },
        id: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            }
          ]
        },
        method: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        params: {
          type: "object",
          additionalProperties: {
            $ref: "#/$defs/Json"
          }
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
    Success: {
      type: "object",
      properties: {
        jsonrpc: {
          const: "2.0"
        },
        id: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            {
              type: "integer",
              minimum: 0,
              maximum: 9007199254740991
            }
          ]
        },
        result: {
          $ref: "#/$defs/Json"
        }
      },
      required: [
        "jsonrpc",
        "id",
        "result"
      ],
      additionalProperties: false
    },
    Failure: {
      type: "object",
      properties: {
        jsonrpc: {
          const: "2.0"
        },
        id: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
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
        error: {
          $ref: "#/$defs/Error"
        }
      },
      required: [
        "jsonrpc",
        "id",
        "error"
      ],
      additionalProperties: false
    },
    Content: {
      oneOf: [
        {
          type: "object",
          properties: {
            encoding: {
              const: "json"
            },
            value: {
              $ref: "#/$defs/Json"
            }
          },
          required: [
            "encoding",
            "value"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            encoding: {
              const: "text"
            },
            value: {
              type: "string"
            }
          },
          required: [
            "encoding",
            "value"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            encoding: {
              const: "base64"
            },
            value: {
              type: "string",
              pattern: "^[A-Za-z0-9+/]*={0,2}$",
              $comment: "Objects.decode verifies canonical base64 padding by round trip. Avoid repeated regex groups, which overflow the stack on valid multi-megabyte content."
            }
          },
          required: [
            "encoding",
            "value"
          ],
          additionalProperties: false
        }
      ]
    },
    Owner: {
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
              pattern: "^[a-z][a-z0-9-]*$"
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
    RetentionPolicy: {
      type: "object",
      properties: {
        objects: {
          oneOf: [
            {
              type: "object",
              properties: { mode: { enum: ["retain", "owned"] } },
              required: ["mode"],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                mode: { const: "expire" },
                maximumAgeDays: { type: "integer", minimum: 1, maximum: 9007199254740991 }
              },
              required: ["mode", "maximumAgeDays"],
              additionalProperties: false
            }
          ]
        },
        revisions: {
          oneOf: [
            {
              type: "object",
              properties: { mode: { enum: ["all", "current"] } },
              required: ["mode"],
              additionalProperties: false
            },
            {
              type: "object",
              properties: {
                mode: { const: "bounded" },
                maximumCount: { type: "integer", minimum: 1, maximum: 9007199254740991 },
                maximumAgeDays: { type: "integer", minimum: 1, maximum: 9007199254740991 }
              },
              required: ["mode"],
              additionalProperties: false,
              anyOf: [{ required: ["maximumCount"] }, { required: ["maximumAgeDays"] }]
            }
          ]
        }
      },
      required: ["objects", "revisions"],
      additionalProperties: false
    },
    RevisionReference: {
      type: "object",
      properties: {
        objectId: { $ref: "#/$defs/Identifier" },
        revision: { type: "integer", minimum: 1, maximum: 9007199254740991 }
      },
      required: ["objectId", "revision"],
      additionalProperties: false
    },
    RevisionReferences: {
      type: "object",
      maxProperties: 256,
      propertyNames: { pattern: "^[A-Za-z][A-Za-z0-9_.-]{0,127}$" },
      additionalProperties: { $ref: "#/$defs/RevisionReference" }
    },
    DataContract: {
      type: "object",
      properties: {
        key: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        version: {
          $ref: "#/$defs/ContractVersion"
        },
        owner: {
          $ref: "#/$defs/Owner"
        },
        mediaType: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        retention: {
          $ref: "#/$defs/RetentionPolicy"
        },
        specMarkdown: {
          type: "string",
          maxLength: 65536
        },
        jsonSchema: {
          type: [
            "object",
            "boolean"
          ]
        }
      },
      required: [
        "key",
        "version",
        "owner",
        "mediaType",
        "retention",
        "specMarkdown"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              mediaType: {
                pattern: "^application/(?:json|[A-Za-z0-9!#$&^_.+-]+\\+json)$"
              }
            },
            required: [
              "mediaType"
            ]
          },
          then: {
            required: [
              "jsonSchema"
            ]
          },
          else: {
            not: {
              required: [
                "jsonSchema"
              ]
            }
          }
        }
      ]
    },
    ToolDefinition: {
      type: "object",
      properties: {
        namespace: {
          type: "string",
          pattern: "^[a-z][a-z0-9-]*$"
        },
        name: {
          type: "string",
          pattern: "^[A-Za-z][A-Za-z0-9_./-]*$",
          maxLength: 192
        },
        interfaceVersion: {
          $ref: "#/$defs/ContractVersion"
        },
        description: {
          type: "string",
          minLength: 1,
          maxLength: 8192
        },
        inputSchema: {
          type: [
            "object",
            "boolean"
          ]
        },
        outputSchema: {
          type: [
            "object",
            "boolean"
          ]
        },
        discovery: {
          type: "object",
          properties: {
            mcp: {
              type: "object",
              description: "Explicit service-owned MCP publication. Omit for internal tools.",
              properties: {
                name: {
                  type: "string",
                  pattern: "^[a-z][a-z0-9_]*$",
                  maxLength: 64
                },
                surface: {
                  enum: [
                    "ivy",
                    "ivy_dev"
                  ],
                  description: "MCP endpoint that publishes this facade. Omitted publications are development-only."
                }
              },
              required: [
                "name"
              ],
              additionalProperties: false
            },
            group: { type: "string", minLength: 1, maxLength: 256 },
            summary: { type: "string", minLength: 1, maxLength: 320 },
            keywords: {
              type: "array",
              items: { type: "string", minLength: 1, maxLength: 64 },
              uniqueItems: true,
              maxItems: 32
            }
          },
          required: [],
          additionalProperties: false
        },
        annotations: {
          type: "object",
          properties: {
            title: {
              type: "string",
              minLength: 1,
              maxLength: 256
            },
            readOnlyHint: {
              type: "boolean"
            },
            destructiveHint: {
              type: "boolean"
            },
            idempotentHint: {
              type: "boolean"
            },
            openWorldHint: {
              type: "boolean"
            }
          },
          required: [],
          additionalProperties: false
        },
        nativeMethod: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        nativeSchemaIdentity: {
          type: "string",
          minLength: 1,
          maxLength: 256
        }
      },
      required: [
        "namespace",
        "name",
        "interfaceVersion",
        "description",
        "inputSchema",
        "outputSchema"
      ],
      additionalProperties: false,
      dependentRequired: {
        nativeMethod: [
          "nativeSchemaIdentity"
        ],
        nativeSchemaIdentity: [
          "nativeMethod"
        ]
      }
    },
    ToolCall: {
      type: "object",
      properties: {
        expectedCallerPrincipalId: { $ref: "#/$defs/Identifier" },
        qualifiedName: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        arguments: {
          $ref: "#/$defs/Json"
        },
        expectedDefinitionHash: {
          $ref: "#/$defs/Hash"
        },
        operationId: {
          $ref: "#/$defs/Identifier"
        },
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        serviceName: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        resourceRef: {
          $ref: "#/$defs/ResourceRef"
        }
      },
      required: [
        "qualifiedName",
        "arguments",
        "expectedDefinitionHash"
      ],
      additionalProperties: false
    },
    ObjectWrite: {
      type: "object",
      properties: {
        mutationId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        contractVersion: {
          $ref: "#/$defs/ContractVersion"
        },
        content: {
          $ref: "#/$defs/Content"
        },
        references: {
          $ref: "#/$defs/RevisionReferences"
        },
        create: {
          type: "object",
          properties: {
            contractKey: {
              type: "string",
              minLength: 1,
              maxLength: 192
            },
            parentId: {
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
            name: {
              type: "string",
              minLength: 1,
              maxLength: 255
            },
            ownerObjectId: {
              anyOf: [
                { $ref: "#/$defs/Identifier" },
                { type: "null" }
              ]
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
            }
          },
          required: [
            "contractKey",
            "parentId",
            "name",
            "ownerObjectId"
          ],
          additionalProperties: false
        },
        objectId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        expectedRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        }
      },
      required: [
        "mutationId",
        "contractVersion",
        "content",
        "references"
      ],
      additionalProperties: false,
      oneOf: [
        {
          required: [
            "create"
          ],
          not: {
            anyOf: [
              {
                required: [
                  "objectId"
                ]
              },
              {
                required: [
                  "expectedRevision"
                ]
              }
            ]
          }
        },
        {
          required: [
            "objectId",
            "expectedRevision"
          ],
          not: {
            required: [
              "create"
            ]
          }
        }
      ]
    },
    DeploymentRecord: {
      type: "object",
      properties: {
        deploymentId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        instanceId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        componentId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        requestHash: {
          $ref: "#/$defs/Hash"
        },
        previousBuild: {
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
        targetBuild: {
          anyOf: [
            {
              type: "string",
              minLength: 1,
              maxLength: 1024
            },
            {
              type: "null"
            }
          ]
        },
        observedBuild: {
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
        phase: {
          enum: [
            "preparing",
            "prepared",
            "checking",
            "draining",
            "activating",
            "verifying",
            "succeeded",
            "rolling_back",
            "rolled_back",
            "failed",
            "needs_attention"
          ]
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
        readiness: {
          type: "object",
          properties: {
            state: {
              enum: [
                "not_run",
                "passed",
                "failed",
                "unknown"
              ]
            },
            message: {
              type: "string",
              maxLength: 4096
            }
          },
          required: [
            "state",
            "message"
          ],
          additionalProperties: false
        },
        errorCode: {
          type: "string",
          minLength: 1,
          maxLength: 128
        }
      },
      required: [
        "deploymentId",
        "hostId",
        "instanceId",
        "componentId",
        "requestHash",
        "previousBuild",
        "targetBuild",
        "observedBuild",
        "phase",
        "createdAt",
        "updatedAt",
        "readiness"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              phase: {
                enum: [
                  "prepared",
                  "checking",
                  "draining",
                  "activating",
                  "verifying",
                  "succeeded",
                  "rolling_back",
                  "rolled_back"
                ]
              }
            },
            required: [
              "phase"
            ]
          },
          then: {
            properties: {
              targetBuild: {
                type: "string",
                minLength: 1,
                maxLength: 1024
              }
            }
          }
        }
      ]
    },
    QueryPredicate: {
      oneOf: [
        {
          type: "object",
          properties: {
            op: {
              const: "contains"
            },
            field: {
              type: "string",
              pattern: "^(object\\.(id|parentId|ownerObjectId|name|path|position|icon|contractKey|revision|contractVersion|archivedAt|effectivelyArchived|createdAt|updatedAt)|data:/.*)$",
              maxLength: 512
            },
            value: {
              type: "string",
              maxLength: 4096
            }
          },
          required: [
            "op",
            "field",
            "value"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            op: {
              enum: [
                "eq",
                "ne",
                "gt",
                "gte",
                "lt",
                "lte"
              ]
            },
            field: {
              type: "string",
              pattern: "^(object\\.(id|parentId|ownerObjectId|name|path|position|icon|contractKey|revision|contractVersion|archivedAt|effectivelyArchived|createdAt|updatedAt)|data:/.*)$",
              maxLength: 512
            },
            value: {
              type: [
                "null",
                "boolean",
                "number",
                "string"
              ]
            }
          },
          required: [
            "op",
            "field",
            "value"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            op: {
              const: "in"
            },
            field: {
              type: "string",
              pattern: "^(object\\.(id|parentId|ownerObjectId|name|path|position|icon|contractKey|revision|contractVersion|archivedAt|effectivelyArchived|createdAt|updatedAt)|data:/.*)$",
              maxLength: 512
            },
            value: {
              type: "array",
              items: {
                type: [
                  "null",
                  "boolean",
                  "number",
                  "string"
                ]
              },
              minItems: 1,
              maxItems: 200
            }
          },
          required: [
            "op",
            "field",
            "value"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            op: {
              const: "isNull"
            },
            field: {
              type: "string",
              pattern: "^(object\\.(id|parentId|ownerObjectId|name|path|position|icon|contractKey|revision|contractVersion|archivedAt|effectivelyArchived|createdAt|updatedAt)|data:/.*)$",
              maxLength: 512
            }
          },
          required: [
            "op",
            "field"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            op: {
              enum: [
                "and",
                "or"
              ]
            },
            args: {
              type: "array",
              items: {
                $ref: "#/$defs/QueryPredicate"
              },
              minItems: 1,
              maxItems: 64
            }
          },
          required: [
            "op",
            "args"
          ],
          additionalProperties: false
        },
        {
          type: "object",
          properties: {
            op: {
              const: "not"
            },
            arg: {
              $ref: "#/$defs/QueryPredicate"
            }
          },
          required: [
            "op",
            "arg"
          ],
          additionalProperties: false
        }
      ]
    },
    ObjectQuery: {
      type: "object",
      properties: {
        contractKey: {
          type: "string",
          minLength: 1,
          maxLength: 192
        },
        contractVersions: {
          type: "array",
          items: {
            $ref: "#/$defs/ContractVersion"
          },
          minItems: 1,
          maxItems: 64,
          uniqueItems: true
        },
        where: {
          $ref: "#/$defs/QueryPredicate"
        },
        select: {
          type: "array",
          items: {
            type: "string",
            pattern: "^(object\\.(id|parentId|ownerObjectId|name|path|position|icon|contractKey|revision|contractVersion|archivedAt|effectivelyArchived|createdAt|updatedAt)|data:/.*)$",
            maxLength: 512
          },
          minItems: 1,
          maxItems: 40,
          uniqueItems: true
        },
        orderBy: {
          type: "array",
          items: {
            type: "object",
            properties: {
              field: {
                type: "string",
                pattern: "^(object\\.(id|parentId|ownerObjectId|name|path|position|icon|contractKey|revision|contractVersion|archivedAt|effectivelyArchived|createdAt|updatedAt)|data:/.*)$",
                maxLength: 512
              },
              direction: {
                enum: [
                  "asc",
                  "desc"
                ]
              }
            },
            required: [
              "field",
              "direction"
            ],
            additionalProperties: false
          },
          minItems: 1,
          maxItems: 3
        },
        includeArchived: {
          type: "boolean"
        },
        limit: {
          type: "integer",
          minimum: 1,
          maximum: 200
        },
        cursor: {
          type: "string",
          minLength: 1,
          maxLength: 8192
        }
      },
      required: [
        "contractKey"
      ],
      additionalProperties: false
    },
    ServiceConnect: {
      type: "object",
      properties: {
        serviceNodeId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        hostId: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        serviceName: {
          type: "string",
          pattern: "^[a-z][a-z0-9-]*$",
          maxLength: 64
        },
        instanceMode: {
          enum: ["singleton", "multiple"]
        },
        version: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        buildId: {
          $ref: "#/$defs/Hash"
        },
        hiveProtocol: {
          const: 1
        },
        nativeVersion: {
          type: "string",
          minLength: 1,
          maxLength: 256
        }
      },
      required: [
        "serviceNodeId",
        "hostId",
        "serviceName",
        "version",
        "buildId",
        "hiveProtocol"
      ],
      additionalProperties: false
    },
    ContractRequirement: {
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
            $ref: "#/$defs/ContractVersion"
          },
          uniqueItems: true,
          maxItems: 64
        },
        writeVersions: {
          type: "array",
          items: {
            $ref: "#/$defs/ContractVersion"
          },
          uniqueItems: true,
          maxItems: 64
        },
        readScope: {
          $ref: "#/$defs/ContractReadScope"
        }
      },
      required: [
        "key",
        "readVersions",
        "writeVersions"
      ],
      additionalProperties: false
    },
    TopicEventKind: {
      type: "object",
      properties: {
        kind: { type: "string", minLength: 1, maxLength: 256 },
        title: { type: "string", minLength: 1, maxLength: 256 },
        description: { type: "string", minLength: 1, maxLength: 2048 }
      },
      required: ["kind", "title", "description"],
      additionalProperties: false
    },
    TopicDefinition: {
      type: "object",
      properties: {
        topic: { type: "string", minLength: 1, maxLength: 192 },
        version: { $ref: "#/$defs/ContractVersion" },
        title: { type: "string", minLength: 1, maxLength: 256 },
        description: { type: "string", minLength: 1, maxLength: 2048 },
        payloadSchema: { type: ["object", "boolean"] },
        eventKinds: {
          type: "array",
          items: { $ref: "#/$defs/TopicEventKind" },
          maxItems: 128
        }
      },
      required: ["topic", "version", "title", "description", "payloadSchema", "eventKinds"],
      additionalProperties: false
    },
    NamespaceDefinition: {
      type: "object",
      properties: {
        namespace: {
          type: "string",
          pattern: "^[a-z][a-z0-9-]*$"
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
        tools: {
          type: "array",
          items: {
            $ref: "#/$defs/ToolDefinition"
          },
          maxItems: 2e3
        },
        discoveryGroups: {
          type: "array",
          items: {
            type: "object",
            properties: {
              group: { type: "string", minLength: 1, maxLength: 256 },
              description: { type: "string", minLength: 1, maxLength: 8192 }
            },
            required: ["group", "description"],
            additionalProperties: false
          },
          maxItems: 128
        },
        notifications: {
          type: "array",
          items: {
            type: "object",
            properties: {
              name: { type: "string", minLength: 1, maxLength: 192 },
              version: { $ref: "#/$defs/ContractVersion" },
              description: { type: "string", minLength: 1, maxLength: 8192 },
              payloadSchema: { type: ["object", "boolean"] }
            },
            required: ["name", "version", "description", "payloadSchema"],
            additionalProperties: false
          },
          maxItems: 128
        },
        topics: {
          type: "array",
          items: { $ref: "#/$defs/TopicDefinition" },
          maxItems: 128
        },
        inventoryKinds: {
          type: "array",
          items: {
            type: "object",
            properties: {
              kind: {
                type: "string",
                minLength: 1,
                maxLength: 64
              },
              version: {
                $ref: "#/$defs/ContractVersion"
              },
              summarySchema: {
                type: [
                  "object",
                  "boolean"
                ]
              },
              searchPointers: {
                type: "array",
                items: { type: "string", pattern: "^/(?:[^~/]|~[01])*(?:/(?:[^~/]|~[01])*)*$", maxLength: 2048 },
                uniqueItems: true,
                maxItems: 32
              },
              archivedPointer: { type: "string", pattern: "^/(?:[^~/]|~[01])*(?:/(?:[^~/]|~[01])*)*$", maxLength: 2048 },
              recencyPointer: { type: "string", pattern: "^/(?:[^~/]|~[01])*(?:/(?:[^~/]|~[01])*)*$", maxLength: 2048 }
            },
            required: [
              "kind",
              "version",
              "summarySchema"
            ],
            additionalProperties: false
          },
          maxItems: 128
        }
      },
      required: [
        "namespace",
        "description",
        "guideMarkdown",
        "tools",
        "topics",
        "inventoryKinds"
      ],
      additionalProperties: false
    },
    RegistrySync: {
      type: "object",
      properties: {
        discoveryHint: {
          type: "string",
          maxLength: 65536
        },
        mcpPrefix: {
          type: "string",
          pattern: "^[a-z][a-z0-9_]*$",
          maxLength: 64
        },
        namespaces: {
          type: "array",
          items: {
            $ref: "#/$defs/NamespaceDefinition"
          },
          maxItems: 128
        },
        contracts: {
          type: "array",
          items: {
            $ref: "#/$defs/DataContract"
          },
          maxItems: 512
        },
        requiredContracts: {
          type: "array",
          items: {
            $ref: "#/$defs/ContractRequirement"
          },
          maxItems: 128
        }
      },
      required: [
        "namespaces",
        "contracts",
        "requiredContracts"
      ],
      additionalProperties: false
    },
    EventFilter: {
      type: "object",
      properties: {
        topics: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 192
          },
          minItems: 1,
          maxItems: 128,
          uniqueItems: true
        },
        sources: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 256
          },
          minItems: 1,
          maxItems: 128,
          uniqueItems: true
        },
        objectIds: {
          type: "array",
          items: { $ref: "#/$defs/Identifier" },
          minItems: 1,
          maxItems: 128,
          uniqueItems: true
        }
      },
      required: [],
      additionalProperties: false
    },
    EventSubscribe: {
      type: "object",
      properties: {
        name: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        filter: {
          $ref: "#/$defs/EventFilter"
        },
        initialSequence: {
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
        "name",
        "filter"
      ],
      additionalProperties: false
    },
    EventAck: {
      type: "object",
      properties: {
        name: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        throughSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        gapThroughSequence: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "name",
        "throughSequence"
      ],
      additionalProperties: false
    },
    Diagnostic: {
      type: "object",
      properties: {
        code: {
          type: "string",
          minLength: 1,
          maxLength: 128
        },
        resource: {
          $ref: "#/$defs/Json"
        },
        severity: {
          enum: [
            "info",
            "warning",
            "error"
          ]
        },
        source: {
          type: "string",
          minLength: 1,
          maxLength: 256
        },
        firstObservedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        lastObservedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        },
        status: {
          enum: [
            "current",
            "stale",
            "unknown",
            "resolved"
          ]
        },
        message: {
          type: "string",
          minLength: 1,
          maxLength: 4096
        }
      },
      required: [
        "code",
        "resource",
        "severity",
        "source",
        "firstObservedAt",
        "lastObservedAt",
        "status",
        "message"
      ],
      additionalProperties: false
    },
    ServiceHeartbeat: {
      type: "object",
      properties: {
        ready: {
          type: "boolean"
        },
        diagnostics: {
          type: "array",
          items: {
            $ref: "#/$defs/Diagnostic"
          },
          maxItems: 100
        }
      },
      required: [
        "ready",
        "diagnostics"
      ],
      additionalProperties: false
    },
    InventoryRevisionConflictDetails: {
      type: "object",
      properties: {
        serviceNodeId: {
          $ref: "#/$defs/Identifier"
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
        currentRevision: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        }
      },
      required: [
        "serviceNodeId",
        "namespace",
        "kind",
        "currentRevision"
      ],
      additionalProperties: false
    },
    InventoryEntry: {
      type: "object",
      properties: {
        nativeId: {
          type: "string",
          minLength: 1,
          maxLength: 1024
        },
        summary: {
          $ref: "#/$defs/Json"
        },
        observedAt: {
          type: "string",
          minLength: 1,
          maxLength: 64
        }
      },
      required: [
        "nativeId",
        "summary",
        "observedAt"
      ],
      additionalProperties: false
    },
    InventorySync: {
      type: "object",
      properties: {
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
        schemaVersion: {
          $ref: "#/$defs/ContractVersion"
        },
        mode: {
          enum: [
            "snapshot",
            "delta"
          ]
        },
        snapshotRevision: {
          type: "integer",
          minimum: 1,
          maximum: 9007199254740991
        },
        expectedRevision: {
          type: "integer",
          minimum: 0,
          maximum: 9007199254740991
        },
        entries: {
          type: "array",
          items: {
            $ref: "#/$defs/InventoryEntry"
          },
          maxItems: 5e3
        },
        removedNativeIds: {
          type: "array",
          items: {
            type: "string",
            minLength: 1,
            maxLength: 1024
          },
          maxItems: 5e3
        }
      },
      required: [
        "namespace",
        "kind",
        "schemaVersion",
        "mode",
        "snapshotRevision",
        "entries"
      ],
      additionalProperties: false,
      allOf: [
        {
          if: {
            properties: {
              mode: {
                const: "delta"
              }
            },
            required: [
              "mode"
            ]
          },
          then: {
            required: [
              "expectedRevision",
              "removedNativeIds"
            ]
          },
          else: {
            not: {
              anyOf: [
                {
                  required: [
                    "expectedRevision"
                  ]
                },
                {
                  required: [
                    "removedNativeIds"
                  ]
                }
              ]
            }
          }
        }
      ]
    },
    ContractReadScope: {
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
  }
};

export {
  digest,
  hashJson,
  hive_wire_schema_default
};
//# sourceMappingURL=chunk-KI5VCYRK.js.map
