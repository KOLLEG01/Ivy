# WhatsApp through a connected WhatsRust MCP server

This collector uses the same direct native MCP transport as the Microsoft 365
example. It starts no model turn and has access only to status, event history and
local unread observations. Configure the authenticated MCP server on the selected
native owner first. No credentials belong in the task script or configuration.

Materialize `whatsrust.task.json` with the [task helper](../../../../tools/operations/data-collector-task.mjs).
Set the exact account JID returned by `whatsrust_events` (including its device
suffix), database identity, native owner, registered project directory and account
key. Copy existing sender and conversation exclusions before changing readers.
Sender comparisons normalize device suffixes and legacy `@c.us` addresses.

Start disabled and inspect one manual run. The first complete event traversal
establishes a baseline without announcing old messages. Subsequent polls retain
incoming message identities, bounded excerpts and content revisions. Event cursors
and pending work survive worker restarts. A changed account, database, journal gap
or invalid page prevents cursor advancement. A capped traversal retains progress
but remains incomplete until catch-up finishes. Resolve a gap explicitly; resetting
the state silently would discard unassessed activity. At most 100 pending messages
are retained; reaching the configured limit blocks advancement with `pending_limit`.
Resolve capacity explicitly rather than discarding messages. Excerpts are limited
to 500 characters; retrieve originals through MCP for assessment.

Before publishing complete activity, each pending conversation is queried again
with `whatsrust_unread`. Reads send no read receipts. A connected local unread flag
with an observation timestamp can be known while the provider's total `count` is
null and `complete` is false. This example requires the flag, never invents the
total, and reports `unreadCount: null`. Its `pendingUnreadCount` covers only incoming
messages retained since the baseline. Neither this result nor the local archive
claims complete phone history or an authoritative remote unread snapshot.

Watch `/data/activity/whatsapp:personal` with the disabled
[Secretary recipe](../../secretary/whatsapp-mcp.assignment.json), replacing the
result object and account key. Stable identities and content hashes avoid repeated
assessments caused by poll timestamps. The fresh observation gate blocks incomplete
or stale scans. Keep one active Secretary assignment for this WhatsApp account;
preserve the former assignment and notification rules for rollback. Other accounts
and collectors need no change.

Secretary retrieves originals through the same MCP server. A media reference or
caption is not proof of its contents: require available original bytes, bounded
chunks, declared size, MIME and SHA-256; report unavailable or unsupported media.
The MCP archive may omit quote/reaction projections and outgoing original media.
Do not treat an API acknowledgement as delivery or a send receipt as a read receipt.
