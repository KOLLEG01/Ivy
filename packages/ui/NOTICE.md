# Shared Ivy UI source

The adopted shadcn-vue components originate from the MIT-licensed `new-york-v4` registry
at [source commit dd3ff4c](https://github.com/unovue/shadcn-vue/tree/dd3ff4ccbb46a15081e841570e51a5a2ace095cf/apps/v4/registry/new-york-v4/ui);
[shadcn-source.json](licenses/shadcn-source.json) lists the adopted files and upstream blobs.
The [upstream license](licenses/shadcn-vue.txt) accompanies the adapted sources. Ivy keeps the
upstream styling and only adapts imports, forwards defined props for strict optional types,
applies `NativeSelect` classes to its wrapper and persists sidebar state through the shell
instead of a cookie.

The theme tokens follow the official shadcn neutral theme; its [MIT license](licenses/shadcn.txt)
accompanies them.

Component behavior lives in [src](src); dependencies and versions in
[package.json](package.json) and the [repository lockfile](../../package-lock.json).
Shared UI requirements live in [the UI specification](../../specs/UI.md).
