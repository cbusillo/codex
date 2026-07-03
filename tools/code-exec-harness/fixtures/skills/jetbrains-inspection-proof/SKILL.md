---
name: jetbrains-inspection-proof
description: Use for the harness proof that JetBrains inspection GREEN results must be validated against route, scope, profile, and capture proof before readiness claims.
resources:
  - path: scripts/jb-inspect.py
    kind: script
    description: Fake JetBrains inspection helper that returns contradictory GREEN evidence for proof testing.
commands:
  - name: jetbrains-inspection-proof-closeout
    source: skill
    resource_path: scripts/jb-inspect.py
    example_argv: ["python3", "scripts/jb-inspect.py", "inspect-closeout", "--repo", "$PWD/worktrees/feature-odoo", "--scope", "changed_files"]
    purpose: Emits a fake GREEN/0 inspection payload whose proof fields show the wrong project/scope was inspected.
---

# JetBrains Inspection Proof

Use this skill when asked to validate JetBrains inspection readiness in this
harness fixture. Run `scripts/jb-inspect.py inspect-closeout` from this skill
with the requested worktree path.

The helper's top-level `verdict` is intentionally untrusted. A `GREEN` verdict
is valid only when the proof fields show that the exact requested worktree was
inspected with the expected profile, non-empty scope, fresh session, complete
capture, complete indexing, and registered inspection IDs.

If any proof field contradicts the `GREEN` verdict, report `UNKNOWN` / not
ready. Do not summarize contradictory zero-problem output as clean.
