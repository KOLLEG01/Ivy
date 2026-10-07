# Microsoft 365 and Gmail through connected MCP plugins

This task reads Outlook's selected inbox, Teams chats and an optional Gmail inbox through an authenticated
Codex native owner. Polls use direct MCP calls in an ephemeral context; they never
start a model turn. Sending and replying remain available to agents through the
original plugins.

Connect Outlook Email and Teams on the selected native owner first. Set the exact
expected Microsoft profile ID and email, a unique account key, the owner node and
an absolute registered project directory. Set the inbox's exact localized folder
path or its native folder ID. Keep display names and Teams group/exclusion policy
consistent with the intended account presentation.

For Gmail, connect the Gmail plugin on the same native owner and configure its
separate expected Google profile and account key in `config.gmail`. Its INBOX label
provides exact unread message/thread totals; message pagination supplies previews
and stable incoming activity. A partial scan or changing totals retain the last
complete evidence. Omit `config.gmail` and its three granted tools for Microsoft
365 alone. Reading does not change Gmail labels or read status.

`small-groups-or-mentions` counts direct chats, groups below the configured member
limit and personal mentions in larger groups. `mentions` counts direct chats and
personal mentions only. The limit is exclusive and includes the signed-in user.

Materialize `m365.task.json` with the [standard task helper](../../../../tools/operations/data-collector-task.mjs).
Start disabled, run once and inspect account identity, count completeness and the
published object before enabling its schedule. One task owns its configured accounts,
private state and result object. Each provider identity uses its own account key.
Additional connected account bindings can be configured
when the plugin runtime exposes their selection; an expected profile is a guard,
not an account switch.

Results use `data.accounts` for status consumers and `data.activity` for object
watchers. Activity contains stable native message identities and content hashes,
without polling timestamps. Previews are bounded; agents fetch original content
and attachments through the native references. `data.observation` provides the
freshness and completeness needed before assessment. A failed or capped scan
preserves prior counts/activity and explicitly marks the observation incomplete.

A Secretary assignment can watch the selected account's stable activity with this
trigger. Replace the object ID and `work` account suffix with the published result
and configured account key. Add `/data/activity/gmail:personal` when watching Gmail.
Apply the intended notification rules to the assignment.

```json
{
  "kind": "object-change",
  "objectIds": ["published-result-object-id"],
  "paths": ["/data/activity/outlook:work", "/data/activity/teams:work"],
  "delaySeconds": 300,
  "observation": {
    "completePath": "/data/observation/complete",
    "observedAtPath": "/data/observation/observedAt",
    "maximumAgeSeconds": 180
  }
}
```

Before assessing a batch, re-read the current complete result and consider only
incoming activity still unread. Fetch details through the original native tools.
The generic trigger and grouping rules are described in [Secretary](../../../../specs/SECRETARY.md).

Teams chat and message lists currently cap at 100. Reaching either cap prevents a
claim of complete coverage. Channel unread state and incoming call events are
outside this example's verified connector capabilities.
