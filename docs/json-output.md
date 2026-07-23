# JSON output mode (`--json`)

`--json` is a top-level flag accepted both before and after the subcommand
(the same placement rule as `--force`, `--dry-run`, `--config`, `--workspace`,
and `--verbose`). It switches a result-producing command from its
human-readable rendering to a single machine-readable JSON document on stdout.

## stdout / stderr / exit guarantees

- **stdout** carries exactly one JSON document, UTF-8, terminated by a
  newline. No ANSI sequences, no live progress rendering, no prompts, no
  banner, no extra text. The document is valid JSON even on the error path.
- **stderr** carries diagnostics only (warnings, the `--workspace`-ignored
  notice, clap parse errors). It is never part of the JSON document.
- **exit codes**:
  - `0` — success, including a no-op (nothing found / nothing deleted).
  - `1` — error (missing config file, unreadable project, bad path target,
    parse error, etc.).
  - `2` — approval required: `--json` was given to `clean` (or the default
    run) without `--force` or `--dry-run`, so a destructive action would need
    an interactive approval the noninteractive JSON run cannot ask for.
    Nothing is deleted. The JSON document describes what would need approval.

`--json` never prompts and never broadens deletion authority. A `clean` that
would need a prompt either runs as a preview (`--dry-run`) or is auto-approved
(`--force`); without one of those, it returns the approval-required result and
exits `2`.

## Applicable commands

Every result-producing command emits JSON under `--json`: `list`, `config`,
`discovery`, `classification`, `clean` (and the default run with no
subcommand), `ignore`, `safelist`, and `init`.

## Envelope

Every document shares one envelope:

```json
{
  "version": 1,
  "command": "list",
  "ok": true,
  "result": { ... }
}
```

On any error:

```json
{
  "version": 1,
  "command": "list",
  "ok": false,
  "error": { "message": "config file not found: /path" }
}
```

`command` is the subcommand name. The default run (no subcommand) reports
`"clean"` because it runs the clean flow.

## Per-command `result`

Status strings match `classify::Status::label()`: `no-git`, `no-remote`,
`unpushed`, `wip`, `cleanable`, `clean`.

### `list`

```json
"result": {
  "projects": [
    {"path": "/abs/path", "status": "cleanable", "rank": 5, "reclaimable_bytes": 4096}
  ],
  "cleanable_count": 1,
  "total_reclaimable_bytes": 4096
}
```

`reclaimable_bytes` is present only on `cleanable` rows. `total_reclaimable_bytes`
is the sum across cleanable rows.

### `classification`

```json
"result": {
  "projects": [
    {"path": "/abs/path", "status": "cleanable", "rank": 5}
  ]
}
```

### `discovery`

```json
"result": {
  "projects": [
    {"path": "/abs/path", "marker": ".git"}
  ]
}
```

### `config`

```json
"result": {
  "config_file": "/abs/path",
  "default_mode": "interactive",
  "max_depth": 4,
  "workspace_roots": ["/abs/path"],
  "safe_delete": ["node_modules"],
  "project_markers": [".git", "package.json"],
  "force": false,
  "dry_run": false,
  "verbose": false
}
```

`config_file` is `null` when no config file was found or used.

### `ignore`

```json
"result": {"path": "foo", "ignored": true}
```

### `safelist`

```json
"result": {"path": "foo", "safe": true}
```

### `init`

```json
"result": {"config_file": "/abs/path", "created": true}
```

### `clean` (and the default run)

```json
"result": {
  "approval_required": false,
  "projects": [
    {
      "path": "/abs/path",
      "status": "cleanable",
      "approved": true,
      "deleted_count": 1,
      "reclaimable_bytes": 4096,
      "items": [
        {"path": "target", "is_dir": true, "classification": "safe", "fate": "deleted"}
      ]
    }
  ]
}
```

`classification` is `safe`, `surfaced`, or `protected`. `fate` is one of:

- `deleted` — the run deleted it (`--force`, no `--dry-run`).
- `would-delete` — a preview (`--dry-run`) would delete it.
- `would-prompt` — a real interactive run would ask; the JSON run did not.
- `kept` — protected, or declined/not approved.

Non-cleanable projects appear with their `status` and an empty `items`
array. When `approval_required` is `true`, every cleanable project has
`approved: false`, `deleted_count: 0`, surfaced items carry `would-prompt`,
safe items carry `would-delete`, and nothing is deleted.