# Native dependency notices

`dependencies.json` maps the exact NuGet graph and bundled runtime to their complete notices.
Package copyright/authors and declared license files/expressions come from the downloaded, locked
NuGet packages. Missing older package license metadata is supplemented from upstream sources in
`sources.json`: Common.Logging3.4.1.GA, IPNetwork2.1.2, DNS2.0.1 and mDNS0.27.0 tagged source.
SimpleBase1.3.1's published package points to Apache; the retained Apache2 text with its copyright
comes from the earliest available upstream tag1.7.0, not a claim that tag contains1.3.1 source.
Microsoft legacy reference packages retain the .NET license; the deployed10.0.10 runtime additionally
includes its complete upstream third-party notices. Standard MIT grants follow each package's
published attribution. SIPSorcery's full additional terms are preserved without reclassification.

These files accompany the native release. New/changed dependencies require updating this index;
the publish runner rejects a graph without matching notice entries. No licenses are fetched during
deployment or inferred from executable filenames.

`CodexMicro-MIT.txt` separately accompanies the adapted in-tree USB/IP/HID implementation.
It preserves sepivip/codexdeck and Marcel Pociot's upstream MIT attribution; this is source
adaptation, not a NuGet dependency. The selected codexdeck source commit is
`d8d8e73706f13cd42e1f1535381fd8bf930f6715`.

`WaitingMarimba.txt` records the bundled waiting audio's source and edits;
`VSCO-2-CE-CC0.txt` contains its complete upstream CC0 1.0 dedication.
