# KDP estimated royalties

Materialize `royalties.task.json` with the DataCollector task helper. It is disabled
until its assigned Amazon account and live report shape have been verified.
The script uses Node HTTP requests only. No public KDP API/PAT contract is available;
the report transport follows the account website's undocumented CSRF/XLSX download.

To configure the credential fallback:

1. Use the Amazon email address and password belonging to the KDP publisher account.
   Store them as `kdp_username` and `kdp_password` in the collector's assigned secrets.
   No additional account ID or API registration is required.
2. If that account uses an authenticator, assign its original TOTP setup key as
   `kdp_totp_seed` and add that name to the task's `secretNames`. The key comes from
   the account's existing authenticator setup; a displayed six-digit code is not a
   TOTP seed. Otherwise leave this optional secret unassigned.
3. Materialize the template, configure the host and run it once before enablement.
   Its XLSX parser requests 512 MiB of V8 heap; set the service's
   `maximumMemoryMb` to at least 512 (the default is 256), or the task is capped.
   Verify the selected account and returned currencies/subscription coverage. A CAPTCHA or
   approval error means unattended credentials are insufficient for that account.

The default template assigns `kdp_username` and `kdp_password`. Ordinary Amazon
email/password forms and a standard authenticator OTP form are supported where the
server provides them. To supply a TOTP seed, add `kdp_totp_seed` to `secretNames`.
The selected OTP form must identify TOTP or explicitly request an authenticator
code; alternative-method links do not select SMS or approval. Unidentified methods
still require interaction.
CAPTCHA, SMS, approval and other account-confirmation pages fail with
`interaction_required`; the script does not bypass them or start a browser.
Direct credentials therefore cannot guarantee unattended login on every host.
Form handling and report parsing have fixture coverage; successful account login
and the current live workbook require verification with the assigned account.

An existing account session can be preferred by assigning a raw Cookie header
secret: set `config.cookieSecret` to `kdp_cookie` and add that name to `secretNames`.
Username/password can remain assigned as fallback, or be omitted when using the
session alone. No browser is needed to run the collector. A successful session is
cached atomically as `.auth/kdp.json` in the isolated task directory, with directory
mode 0700 and file mode 0600 on POSIX and a credential fingerprint. Keep that
directory protected on Windows. Cookies, CSRF values, credentials and signed download URLs never appear in
task state, observations or logs. Credential changes invalidate the cached session.

There is no KDP PAT to obtain from its report settings. A Cookie header is an Amazon
account session credential, not an API token; use one only when already supplied
through an authorized account integration. It is optional and the collector never
opens a browser to obtain it. Amazon's sign-in can return a form targeting
`/errors_page/validateCaptcha` instead of an email/password form; that is a concrete
`interaction_required` blocker, not a missing KDP ID or token setting.

Collection covers the current month through today in UTC. The parser prefers
Combined Sales over overlapping format sheets, otherwise uses disjoint ebook,
paperback, hardcover and audiobook royalty sheets. Audiobook sales are included in
`salesEstimatedRoyalties` alongside the other disjoint sales formats. It ignores
summaries and subtotal rows, rejects unknown or incomplete populated detail sheets
and malformed amounts, and keeps currencies separate.

KDP's KENP reporting can combine Kindle Unlimited pages read and Audible Plus pages
listened to. Its month-to-date and Royalties Estimator subscription earnings cannot
be split by book format; see Amazon's
[audiobook reporting documentation](https://kdp.amazon.com/en_US/help/topic/GHJW2N8GLTQLK9TY).
The output therefore uses `subscriptions`, `subscriptionRows`, `subscriptionSheet`
and `subscriptionEstimatedRoyalties` for this shared coverage. Subscription
estimates are included only when an amount is present in the report; pages-only and
missing subscription reports are explicit, and `combinedEstimatedRoyalties` stays
null in those cases. When the Combined Sales and KENP sheets both include
subscription amounts, their currency totals must agree before the duplicate
amounts are omitted. Combined rows labeled `Audible subscription royalties` or
`Audible Plus` count as subscriptions, alongside Kindle Unlimited.
No per-page rate or currency conversion is invented. Authentication, download or
parse failures throw; they never return a previous observation or zero earnings.

These are [estimated royalties](https://kdp.amazon.com/en_US/help/topic/G6BZ4M9PVJ8YSCW8),
not payments or finalized income. KDP estimates can lag and KU rates are finalized
later. Report fields and transport are grounded in the
[download implementation](https://github.com/AAA0109/Amazon-KDP-Royalty/blob/master/background.js)
and [workbook parser](https://github.com/joshyattridge/amazon-kdp-skill/blob/main/lib/parseKdpWorkbook.ts);
those sources use browser sessions, so they do not establish direct-login availability.
