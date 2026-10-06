# Secretary operation

Secretary executes scheduled and event-triggered assignments, retains captured inbox items and routes configured notices and follow-up.
[Requirements](../specs/SECRETARY.md) define capture, media and contact semantics.

New installations have no assignments. The UI offers editable schedule and event drafts; saving and enabling are explicit actions. Existing assignments remain user configuration.
Use **hive_event_topics** to list registered triggers, their providers, event kinds and payload schemas. Filter by service, namespace or topic when needed.

[Settings](../specs/schemas/secretary.schema.json) supplies scope, sources, admitted producer identities and notice policy.
Source producers push observations through the capture/attach/checkpoint API and own their integration configuration. Secretary does not load source-specific adapters.
Protected instance settings select the [Main target](../services/secretary/src/assignment-delivery-config.ts) and [inbox notification age](../services/secretary/src/delivery-config.ts). An assignment can override the configured minimum notification age; new UI drafts explicitly select zero.

Automatic assessment runs through assignments. Inbox capture alone does not start a separate assessment pipeline.
Message events that advertise `messageChannel` are grouped by source and channel key for five minutes from the first message. The publisher supplies an opaque reference and implements the `message-channel.unread` tool. Secretary requests a fresh, complete unread answer before starting one assessment for the group. A read channel is settled without a notice; other channels keep their own windows. If unread evidence is unavailable, the assessment waits and eventually fails instead of sending an unverified notice. Discover the event payload through `hive_event_topics` or the assignment UI.
Read **secretary.status** and source recovery issues first. Use **secretary.operation** with the original ID for incomplete captures, checkpoints or assessments. Media references identify registered contracts and exact revisions; inspect retained evidence before reacquiring a source.

Every assignment returns the same decision format defined by the [runner](../services/secretary/src/assignment-runner.ts). Main and Voice delivery follow current contact rules; results with no notification stay in the journal. The execution journal retains original delivery operations and evidence. Native concurrency is bounded by recordsPerTick; remaining work stays queued.

Main/Voice follow-up uses **secretary_follow_up** with an item or execution object reference and a question. Poll **secretary_follow_up_read** with the original operation ID. Its read-only context uses the retained result and trigger.
