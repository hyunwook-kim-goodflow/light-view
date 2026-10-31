# light-view website

Source of https://lightview.goodflow.dev (Zola). Design: `docs/superpowers/specs/2026-10-31-light-view-site-design.md`.

- Data: `powershell -File scripts/site-data.ps1` (writes `site/data/releases.json` and `site/content/_generated/`).
- Preview: `zola --root site serve`, build: `zola --root site build` (output `site/public/`).
- Checks: `powershell -File scripts/site-check.ps1` after a build.
