# Release notes

One file per version, `<version>.md` (for example `0.3.0.md`), written before the version tag is pushed. The release workflow (`.github/workflows/release.yml`) fails without it, puts its text at the top of the draft GitHub release, and the website (`lightview.goodflow.dev/releases/<version>/`) shows it in each language.

```markdown
---
version: 0.3.0
summary_ko: 한 줄 요약 (릴리스 목록에 보여요)
summary_en: One-line summary (shown in the release list)
signed: false
---
## 한국어

### 새로운 것
- …

### 고친 것
- …

### 알려진 문제
- …

## English

### New
- …

### Fixed
- …

### Known issues
- …
```

- `version` must equal the tag (`v0.3.0` → `0.3.0`).
- `signed: true` exactly when the release is code-signed (SignPath, from 1.0.0); the website shows it. The release workflow fails if it does not match whether that build was signed.
- `## 한국어` comes first, then `## English`; headings inside use `###`.
- Write for people using the app: what they can do now, what was fixed, what still goes wrong. Leave out internal refactors.
