# mxmon for agents

`mxmon` reads live Apple Silicon telemetry (power, frequency, temperature, GPU,
memory, network, per-process energy, SMART, thermal pressure) from macOS
frameworks with no `sudo`, no kexts, and no daemons. This file is the guide for
using it from a script or an AI agent instead of digging through `powermetrics`,
`ioreg`, `top`, `iostat`, and raw SMC keys.

Reach for a verb. Bare `mxmon` opens the TUI; every verb below is headless,
fast, and prints machine output when its stdout is not a terminal.

## Recipes

```sh
mxmon get power.package_w thermal.throttling   # pull specific values, one per line
mxmon snapshot                                 # the whole report (JSON when piped)
mxmon snapshot --only power,thermal            # just those groups
mxmon check 'thermal.throttling == false' && cargo build   # gate on a condition
mxmon watch cpu.self_ratio power.package_w --for 10s        # bounded NDJSON stream
mxmon top power                                # rank processes by a resource
mxmon health                                   # composite verdict, exit 1 if degraded
mxmon explain thermal                          # a plain-language diagnosis
mxmon schema                                   # the full contract, self-describing
```

`get` and `check` share one dot-path grammar: `power.ecpu.cores[0].power_w`,
`processes.top[0].pid`. Array indices work as `[0]` or `.0`.

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

`check` respects this: comparing against a `null` source yields `unknown` (exit
2), not a silent `false`, so `mxmon check 'thermal.cpu_max_c < 90'` never passes
just because the temperature source was down.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success; `check` true; `health` ok |
| 1 | `check` false; `health` warn or crit |
| 2 | usage error, or `check` undecidable (a referenced source was null) |
| 3 | no usable data (all sources down, a timeout, or an unknown path) |
| 4 | a control action was refused or failed |

## Control

`mxmon kill <pid>`, `mxmon signal <SIG> <pid>`, and `mxmon renice <n> <pid>` act
on processes. They confirm interactively by default; pass `--yes` to skip the
prompt and `--dry-run` to preview. Off a terminal (piped or automated) they fail
closed: without `--yes` they refuse and exit 4, so nothing is signaled by
accident. Raising priority or touching another user's process needs `sudo`.

## Notes

- Only meaningful on a real Apple Silicon Mac.
- Read commands settle briefly (they wait for delta-based rates); pass
  `--timeout <dur>` to bound it. `watch` streams until `--for`, `--count`, or a
  closed pipe.
- Add `--format json|ndjson|table|compact` to force a shape, or `--no-color`.
