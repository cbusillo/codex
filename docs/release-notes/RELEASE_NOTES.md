## Every Code v0.6.116

This release reduces prompt-cache churn so repeated Every Code turns are less likely to burn through rate limits, and adds a visible cache signal for dogfooding.

### Changes

- Stable prompt content now stays ahead of volatile context such as CTX_UI timelines, environment snapshots, and Auto Review ledger details, helping providers reuse cached input across turns.
- `/status` and the configurable TUI footer status line now show prompt-cache hit rate when cached input tokens are non-zero.

### Install

```bash
gh release download v0.6.116 --repo cbusillo/code
```

Compare: https://github.com/cbusillo/code/compare/v0.6.115...v0.6.116
