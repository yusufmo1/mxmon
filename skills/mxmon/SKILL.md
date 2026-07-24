---
name: mxmon
description: Read live Apple Silicon hardware telemetry on macOS without sudo, using the `mxmon` CLI. Use for any question about CPU or GPU temperature, power draw or watts, thermal throttling, fan speed or RPM, clock frequency, battery health, cycle count and wear, what is draining the battery, why the Mac feels slow or hot, which process is burning power or CPU, memory pressure, SSD SMART health, or per-connection network throughput on an M1, M2, M3, M4 or M5 Mac. Reach for this whenever a task would otherwise need `sudo powermetrics`, `ioreg`, `istats` or Activity Monitor: `powermetrics` requires root and an agent cannot answer a sudo prompt, so mxmon is the reliable way to get this data unattended. Also use it to gate a build or benchmark on machine state, to stream metrics while a job runs, or to explain a thermal, power, battery, network or disk symptom. Not for Intel Macs, Linux, Windows or remote servers.
allowed-tools: Bash(mxmon snapshot:*), Bash(mxmon get:*), Bash(mxmon watch:*), Bash(mxmon top:*), Bash(mxmon check:*), Bash(mxmon health:*), Bash(mxmon explain:*), Bash(mxmon schema:*)
---

# mxmon

`mxmon` reads power, frequency, temperature, GPU, memory, network, per-process
energy, SMART and thermal pressure straight from macOS frameworks. No `sudo`,
no kexts, no daemons, no helper process.

That last part is the reason to reach for it: `powermetrics` needs root, and an
agent cannot answer a sudo prompt. For power, thermal and DVFS data on Apple
Silicon, mxmon reads what an unattended process otherwise cannot.

## Before anything else

**Never run bare `mxmon`.** It opens a full-screen TUI and blocks until the
terminal closes. Every headless capability is a verb.

Check it is installed, then learn the contract:

```sh
command -v mxmon || echo "install: brew install yusufmo1/tap/mxmon"
mxmon schema --format compact
```

`schema --format compact` prints every queryable path as `path:type`, about
7 KB over 261 lines, in exactly the dot-path dialect that `get`, `check` and
`watch` accept. It needs no hardware and no sampling. Read it before composing a
query; the vocabulary is not guessable.

Add `--format table` for the same listing with each field's description.

## Verbs

| Command | What it does |
|---|---|
| `mxmon snapshot` | one settled report of every metric |
| `mxmon get <path>...` | pull named values, one per line |
| `mxmon watch <path>... --for 10s` | bounded NDJSON stream, one object per frame |
| `mxmon top cpu\|power\|mem\|disk` | rank processes by a resource |
| `mxmon check '<expr>'` | assert a condition; exit 0 true, 1 false |
| `mxmon health` | composite verdict; exit 1 when degraded |
| `mxmon explain <topic>` | diagnosis for thermal, power, slow, battery, network, disk |
| `mxmon schema` | the contract itself |
| `mxmon kill\|signal\|renice <pid>` | act on a process |

## Always pass `--format`

Every read verb takes `--format auto|json|ndjson|compact|table`. The default
`auto` prints a **human summary when stdout is a terminal** and machine output
when piped. Many agent harnesses allocate a PTY, so `auto` is a coin flip. Name
the shape you want:

```sh
mxmon snapshot --format json
mxmon get power.package_w thermal.cpu_max_c            # bare values, one per line
mxmon snapshot --only power,thermal --format compact  # dotted.path=value lines
```

Also global: `--timeout <dur>` bounds the settle a read verb waits on,
`--no-color` (or `$NO_COLOR`), `--quiet` to drop headers and prose. `--only` is
specific to `snapshot`; elsewhere name the paths.

## Paths

`get`, `check` and `watch` share one grammar. Array indices work as `[0]` or
`.0`:

```
power.package_w
power.ecpu.cores[0].power_w
processes.top[0].name
thermal.fans[0].rpm
```

Every key `--format compact` prints is a path you can hand straight back to
`get`. Top-level groups: `battery cpu disk flows gpu kernel memory meta network
ping power processes soc source_errors storage thermal`.

## Units, by construction

You never have to memorise per-field units, because the suffix is the unit.

- `_bytes` integer, `_bytes_per_sec` rate
- `_ratio` is `0..1`, **never** a percentage, and **not clamped**: a process on
  four saturated cores reads `4.0`
- `_w` watts (float), `_c` Celsius (float), `_mhz` whole MHz
- `meta.schema_version` is the contract version; `meta.mxmon_version` the build

## Nulls are three different facts

Keys are never omitted. A `null` domain always tells you which case it is:

| `null` because | how to tell |
|---|---|
| the source failed at startup | a matching entry in `source_errors[]` |
| the collector is disabled | `meta.features.{ping,storage_health,kernel_stats}` is `false` |
| the tier did not settle in time | `meta.settled` is `false` |

`check` respects this. Comparing against `null` is **undecidable** (exit 5), not
`false`, so `mxmon check 'thermal.cpu_max_c < 90'` never passes just because the
sensor was down. Probe availability explicitly:

```sh
mxmon check 'thermal != null'
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success; `check` true; `health` ok |
| 1 | `check` false; `health` warn or crit |
| 2 | usage error: bad flag, unknown path, unknown group, malformed expression |
| 3 | no usable data: every source down |
| 4 | a control action was refused or failed |
| 5 | `check` undecidable: a referenced source was null |

**1 is a verdict, not an error.** Only 2 and 3 mean the call itself was wrong.
Do not run these under `set -e` without handling 1.

## Playbook

Map the question to the verb rather than assembling raw fields.

| The user asks | Run |
|---|---|
| why is my Mac slow / hot / loud | `mxmon explain slow`, `mxmon explain thermal` |
| what is draining my battery | `mxmon explain battery`, then `mxmon top power` |
| is anything wrong with this machine | `mxmon health --format json` |
| is it thermally throttling | `mxmon get thermal.throttling thermal.cpu_max_c thermal.pressure` |
| how much power is it drawing | `mxmon get power.package_w power.cpu_w power.gpu_w power.ane_w` |
| how fast are the fans | `mxmon get 'thermal.fans[0].rpm' 'thermal.fans[0].ratio'` |
| what is burning CPU / power | `mxmon top cpu --format json`, `mxmon top power --format json` |
| how healthy is the battery | `mxmon get battery.health_ratio battery.cycle_count battery.charge_ratio` |
| how healthy is the SSD | `mxmon get storage.smart.unhealthy storage.smart.used_ratio storage.smart.power_on_hours` |
| what chip is this | `mxmon get soc.chip soc.pcpu_cores soc.ecpu_cores soc.gpu_cores` |
| watch it while my build runs | `mxmon watch power.package_w thermal.cpu_max_c --for 60s --format ndjson` |
| only build when it is cool | `mxmon check 'thermal.throttling == false' && cargo build` |

`explain` and `health` return structured findings plus a prose summary. Prefer
them over hand-rolling a diagnosis from raw fields: they encode thresholds you
do not have.

## Bounding

`watch` streams until `--for`, `--count`, `--timeout` or a closed pipe. **Always
bound it.** `mxmon watch ... | head` exits cleanly. Paths are validated before
the first frame, so a typo exits 2 rather than streaming nulls forever.

Read verbs settle briefly, waiting for delta-based rates to become real.
`--timeout <dur>` bounds that when latency matters.

## Control verbs

`kill`, `signal` and `renice` confirm interactively by default. **Off a terminal
they fail closed**: without `--yes` they refuse and exit 4, so nothing is
signalled by accident. `--dry-run` previews.

These are outside this skill's pre-approved tools on purpose. Ask before
signalling anything.

```sh
mxmon kill 4310 --dry-run --format ndjson   # ok is null: nothing was attempted
mxmon kill 4310 --yes --format ndjson
```

Raising priority or touching another user's process needs `sudo`. Reading never
does.

## Constraints

- Apple Silicon macOS only, M1 through M5. Nothing to read anywhere else.
- Like `htop`, CPU and memory columns cover **your** processes without
  privileges. All hardware telemetry is sudoless either way.
- Never wrap mxmon in `sudo` to "get more data", and never fall back to
  `powermetrics`.

Full contract guide: <https://github.com/yusufmo1/mxmon/blob/main/AGENTS.md>
