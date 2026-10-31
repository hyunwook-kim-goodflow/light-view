# light-view website

Source of https://lightview.goodflow.dev (Zola 0.23). Design: `docs/superpowers/specs/2026-10-31-light-view-site-design.md`. Built, checked and deployed by `.github/workflows/site.yml`.

- Data: `powershell -File scripts/site-data.ps1` writes `site/data/` (release data, PRIVACY.md and the Code signing policy text), the release pages in `site/content/releases/` and, when ads are on, `site/static/ads.txt` (all git-ignored).
- Preview: `zola --root site serve`; build: `zola --root site build` (output `site/public/`).
- Checks: `powershell -File scripts/site-check.ps1 -AdsTest -Zola zola` after a build; script tests: `powershell -File scripts/tests/site.Tests.ps1`.
- Release notes: `docs/releases/<version>.md` (format in `docs/releases/README.md`).
- Ads stay off until `[extra.ads]` in `config.toml` is filled in (design §9); analytics until `[extra.analytics] token` is set; the sponsor link until `extra.sponsor_url` is set.
