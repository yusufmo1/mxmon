# mxmon for agents

`mxmon` reads live Apple Silicon telemetry (power, frequency, temperature, GPU,
memory, network, per-process energy, SMART, thermal pressure) from macOS
frameworks with no `sudo`, no kexts, and no daemons. This file is the guide for
using it from a script or an AI agent instead of digging through `powermetrics`,
`ioreg`, `top`, `iostat`, and raw SMC keys.

Reach for a verb. Bare `mxmon` opens the TUI; every verb below is headless,
fast, and prints machine output when its stdout is not a terminal.

## Start here

```sh
mxmon schema --format compact    # every queryable path and its type, about 7 KB
mxmon --help                     # verbs, flags, examples, exit codes
```

`schema --format compact` is the cheapest way to learn the contract: one
`path:type` per line, in exactly the dialect `get`, `check`, and `watch` accept.
Add `--format table` for the same listing with each field's description.

## Recipes

```sh
mxmon get power.package_w thermal.throttling   # pull specific values, one per line
mxmon snapshot                                 # the whole report (JSON when piped)
mxmon snapshot --only power,thermal            # just those groups
mxmon snapshot --format compact                # one greppable line per leaf
mxmon check 'thermal.throttling == false' && cargo build   # gate on a condition
mxmon watch cpu.self_ratio power.package_w --for 10s        # bounded NDJSON stream
mxmon top power                                # rank processes by a resource
mxmon health                                   # composite verdict, exit 1 if degraded
mxmon explain thermal                          # a plain-language diagnosis
```

`get`, `check`, and `watch` share one dot-path grammar:
`power.ecpu.cores[0].power_w`, `processes.top[0].pid`. Array indices work as
`[0]` or `.0`. Every key `--format compact` prints is a path you can hand
straight back to `get`.

## Output shapes

Every read verb takes `--format`:

| Shape | What it emits |
|---|---|
| `auto` | human summary on a terminal, machine output when piped (the default) |
| `json` | pretty-printed JSON |
| `ndjson` | one JSON object per line |
| `compact` | flat `dotted.path=value` lines, one leaf per line |
| `table` | aligned columns |

Pick `compact` when you want to `grep`, `cut`, or line-diff two runs, or when
you want the keys to be paths you can query next. It is not a smaller shape for
the report: repeating the full path on every leaf costs more than JSON's nesting
once an array gets long, so a whole snapshot comes out a few percent larger. For
`schema` it is both, because one `path:type` line replaces a whole JSON Schema
property envelope. When you want fewer bytes from the report, name the paths
(`mxmon get`) or the groups (`--only`) instead of changing shape.

Also global: `--timeout <dur>` bounds the settle a read verb waits on,
`--no-color` suppresses ANSI (as does `$NO_COLOR`), and `--quiet` drops headers,
badges, and prose while keeping data and errors. `--only <groups>` is specific
to `snapshot`; elsewhere, name the paths you want.

`man`, `completions`, and `debug` emit a fixed artifact and ignore the output
flags; their help says so.

## The contract

`snapshot` and `--json` emit the same versioned document. It is consistent by
construction, so you never have to memorize per-field units:

- byte counts are integers ending in `_bytes`; rates end in `_bytes_per_sec`
- ratios are `0..1` (never percentages) and end in `_ratio`; they are not
  clamped, so a process on four cores reads `4.0`
- power is watts (`_w`, float), temperature is Celsius (`_c`, float), frequency
  is whole MHz (`_mhz`)
- `meta.schema_version` is the contract version; `mxmon schema` is the JSON
  Schema, with every field's unit in its `description`

Keys are never omitted. A domain that is `null` has one of three causes, and
you can always tell which:

| `null` because | how to tell |
|---|---|
| the source failed at startup | a matching entry in `source_errors[]` |
| the collector is disabled | `meta.features.{ping,storage_health,kernel_stats}` is `false` |
| the tier did not settle in time | `meta.settled` is `false` |

`check` respects this: comparing against a `null` source is undecidable (exit
5), not a silent `false`, so `mxmon check 'thermal.cpu_max_c < 90'` never passes
just because the temperature source was down. Test availability explicitly with
`mxmon check 'thermal != null'`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success; `check` true; `health` ok |
| 1 | `check` false; `health` warn or crit |
| 2 | usage error: a bad flag, an unknown path, an unknown group, a malformed expression |
| 3 | no usable data: every source down |
| 4 | a control action was refused or failed |
| 5 | `check` undecidable: a referenced source was null |

Code 2 is deliberately the code clap uses for a bad flag. Naming something the
contract does not have is one class of mistake whether the parser or the
selector caught it, which is why an undecidable `check` gets a code of its own
rather than sharing that one.

## Control

`mxmon kill <pid>`, `mxmon signal <SIG> <pid>`, and `mxmon renice <n> <pid>` act
on processes. They confirm interactively by default; pass `--yes` to skip the
prompt and `--dry-run` to preview. Off a terminal (piped or automated) they fail
closed: without `--yes` they refuse and exit 4, so nothing is signaled by
accident. Raising priority or touching another user's process needs `sudo`.

With `--format json` or `ndjson`, both the plan and the result come back as one
object per target, so you can act and then parse what happened:

```sh
$ mxmon kill 4310 --yes --format ndjson
[{"action":"signal","error":null,"name":"node","ok":true,"pid":4310,"signal":"SIGTERM"}]
```

`ok` is `null` in a dry run: nothing was attempted, so neither `true` nor
`false` would be honest.

## Notes

- Only meaningful on a real Apple Silicon Mac.
- Read verbs settle briefly (they wait for delta-based rates to become real);
  `--timeout <dur>` bounds that. `watch` streams until `--for`, `--count`,
  `--timeout`, or a closed pipe, and `mxmon watch ... | head` exits cleanly.
- `watch` validates every path before its first frame, so a typo exits 2 rather
  than streaming nulls at you forever.
