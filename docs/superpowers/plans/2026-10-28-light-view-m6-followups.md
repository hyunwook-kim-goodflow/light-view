# light-view M6 follow-ups

M6 (resize, Explorer sort, long paths) merged from branch m6-resize. Spec: docs/superpowers/specs/2026-10-28-light-view-m6-resize-design.md. Plan: docs/superpowers/plans/2026-10-28-light-view-m6-resize.md.

## Rulings

- Ruling P1: EXIF is attached after fitting, not inside encoders — T1 `encode` takes no exif parameter; T2 adds `pub fn attach_exif(bytes: &[u8], format: OutputFormat, exif: &[u8]) -> Result<Vec<u8>, String>` (JPEG: APP1 right after SOI; WebP: EXIF chunk with VP8X flag; PNG: returns bytes unchanged); fit runs with max_bytes minus the EXIF block length (+ chunk overhead) so the final file still meets the limit; IPC `EncodeWebp` and `lv_webp::encode_rgba` carry no exif. — keeps trials cheap and one metadata path for all formats — if wrong: small rework in T1/T3 signatures.
- Ruling P2: "read-only folder" test uses a temp folder with a deny-write ACE for the current user (icacls, removed in teardown), not the read-only attribute. — the attribute does not block writes on Windows — if wrong: test only.
- Task 1: Ruling: "10%씩" = multiplicative (each step 90% of previous long edge) — matches "10% at a time" in plain reading and terminates the same — if wrong: a few more/fewer steps, test-only change.
- Task 1: Ruling: EXIF budget reduction (P1) is owned by Task 2 (`exif_overhead`) and applied by Task 5 when building the fit budget — keeps fit format-agnostic — if wrong: final file a few hundred bytes over limit; Task 9 smoke checks real files against the limit.
- Task 2: Ruling: exif_overhead may over-estimate by 18 bytes for WebP that already has VP8X; documented as an upper bound — safe for the size budget — if wrong: result 18 bytes smaller than possible.
- Task 3: Ruling: spec §10 wins — replace the per-trial EncodeWebp path with one `FitWebp` request per photo: app sends pixels + Target + start_quality once to one single-use worker; the worker runs lv_core::resize::fit with lv_webp::encode_rgba as the encoder and answers the FitOutcome (bytes, size, quality) or CantFit. EncodeWebp is removed (no other caller). — spec is binding and the per-trial cost is 7–30 process spawns per photo — if wrong: lv-worker now links lv-core (pure Rust), a larger worker binary.
- Task 5: Ruling: 1 MB = 1,000,000 bytes — the smaller reading satisfies sites that mean either MB or MiB — if wrong: results ~5% smaller than allowed.
- Task 5: Ruling: photos arriving mid-save open a fresh session after the save ends (spec §6 closes the screen after save; §10 "added after save") — keeps both rules — if wrong: small routing change.
- Task 5: Ruling: entering resize mode never discards the viewer's pending rotation, from any entry path (menu, drop, forwarded). Leaving without a save returns to entered_from with its pending quarters restored (no prompt). The menu entry also applies those quarters to that photo's resize item (spec §10). — matches "원본은 건드리지 않음" and avoids a new prompt — if wrong: user sees the rotation still pending after leaving, as before entering.
- Task 6: Ruling (I3): thumbnails required — the user approved the mockup with photo tiles; keep a small thumbnail from the estimate decode, number as fallback — if wrong: small memory cost.
- Task 6: Ruling (I4): a mouse pick never moves keyboard focus to a choice; ←/→ change an option only when the choice was focused with Tab — brief makes ←/→ the photo keys — if wrong: keyboard users press Space/Enter-style keys instead.
- Task 6: Ruling (I5): after a save with any failed or can't-fit photo, the screen stays open showing only those photos, each with its reason in the list; the saved ones leave the session; if all succeed, the screen closes as now — satisfies spec §6 "실패한 장은 이유를 목록에 남긴다" and §6 close-after-save — if wrong: one extra Esc for the user.
- Task 7: Ruling: reviewer minor "Player caps at 15 items" not adopted — Microsoft documents MultiSelectModel=Player as shown for any number of items (the 15 limit is the default without the value) — if wrong: README note missing; Task 9 smoke checks >15 selection only if possible without the installer.
- Task 8: Ruling: on rescans a listed file keeps the date/size captured at first listing (stable order; own saves don't reshuffle) — R5 "decided once at open" — if wrong: outside edits don't move a photo in the order until reopen.
- Final: Ruling (I2): fix both halves — stop flag checked between photos + wait up to END_SESSION_WAIT on WM_ENDSESSION then commit; and record folders holding live backups in a small list in the data dir (portable-aware), recovered at every start and pruned when empty — README promise then holds — if wrong: a tiny extra file in the data dir.
- Final: Ruling: residual I2 is load-bearing for the "no way back" anti-goal but no second fix wave runs — surface to the user before merge with a recommended small fix (keep entry until the folder was reached and nothing hidden from ended processes remains; run listed recovery only after the instance guard is held; skip running pids before treating an entry as cut short).

## Deferred (not fixed in M6)

- Task 1: minor (deferred): step-skip heuristic in fit can return one 10% step smaller than strict stepping on grain-heavy images (fit.rs:93-101) — final review to triage.
- Task 1: minor (deferred): resample panics on short buffer instead of EncodeError (geometry.rs:63-79).
- Task 1: minor (deferred): CantFit doc overstates floor (fit.rs:23-25); Fitted.quality meaningless for PNG (Task 5 must not show it); opacity rescan per step; vacuous `size.0 <= 4000` assertion (fit.rs:204); slow 4000x3000 debug tests.
- Task 2: minor (deferred): untested JPEG existing-EXIF drop path and WebP before-XMP branch (meta.rs:290-341); EXIF APP1 inserted before JFIF APP0 (exif_edit inserts after); VP8X payload length unchecked; quality estimate tests only image-encoder tables, uses table ids 0/1; gps test could assert no GPS-context field at all.
- Task 3: minor (deferred): 40 B/px budget ~20% above measured worst case, Rust alloc failure aborts worker; duplicate link directive lv-webp build.rs/lib.rs; redundant import-check union in package.ps1; libsharpyuv shipped but unused by simple API; oversize-output branch untested.
- Task 3: minor (deferred): fit peak during quality bisection unmeasured in Job; check() does not verify Fitted size/quality for resolution targets (taken-over worker could shrink); stale doc comments (lv-worker tests "+17", protocol.rs header).
- Task 4: minor (deferred): flush timeout can misreport kept_at/ResultNotRecycled; non-exists move-back failure reported as NameTaken; kept_at may name a vanished file; recovery retries unrenamable backups every start; result deleted before undo reported as ResultChanged; stale same-pid backup → NameTaken; save_new does not retry taken temp name.
- Task 5: minor (deferred): over-limit new file that cannot be deleted still reported OverLimit; save's decode can run beside 2 estimates (3 decodes); SHQueryRecycleBinW and identity capture on UI thread; rotated RGBA kept through fit, HEIF bytes cloned; forwarded folder adds only first file; partial forward failure duplicates paths in two windows; src_size (0,0) after decode failure; HEIF scaled_from always None; peak memory may reach ~2x budget (Task 9 to measure).
- Task 6: minor (deferred): [저장 (0장)] pressable when only 맞추기 불가 photos remain.
- Task 6: minor (deferred): M1 maximized/fullscreen short window still overlaps sidebar; GPU thumb bitmaps outlive session and re-upload per setting change; tile corners square over rounded cell.
- Task 6: minor (deferred): short-window sidebar scroll follows keyboard focus only (no wheel/drag); tile mask assumes paper filmstrip background; strings not on fixed list chosen by implementer (기준/출력/메타데이터, failure reasons) — user to confirm; EDIT UIA names via MSAA only (verify with Narrator in Task 9).
- Task 7: minor (deferred): test does not assert uninsdeletekey absent on sub-lines; R4 race of many simultaneous --resize launches — check in Task 9 smoke.
- Task 8: minor (deferred): same-folder reopen (incl. Explorer double-click) keeps stale Explorer order and re-reads metadata; meta lookups exact-case vs same_path; long-path test misses pixel rotation + go_next; live Explorer test can't distinguish failure vs no window; hung Explorer leaks a thread per folder; long_form sync at startup on stalled share; iphone test always #[ignore].
- Final: fix wave scope: I1 (PROC_THREAD_ATTRIBUTE_JOB_LIST with fallback), I2 (as ruled), I3 (SECURITY.md KO+EN), M2 (WebP deadline scales with pixels), M4 (README worker description), M1 (README note on memory with large photos). Later: M3, M5 and all other deferred minors.
- Final: fix wave re-review — I1, I3, M1, M2, M4 addressed; I2 residual open: recover_listed drops a folder's entry even when the folder could not be reached (offline USB/BitLocker/network at start), so hidden originals there are not recovered later. New minors: start-up can delete a running copy's truncated entry; forwarding launches run recovery and may exit mid-restore (original back but hidden); JOB_LIST_REFUSED set on transient failures; WM_ENDSESSION worst case adds 5 s.
- Final: follow-up round (61e6004..89ebeb7) re-review: all addressed, no new Critical/Important. New minors: enumeration error in a reachable folder drops the entry; leftover .tmp entry files never cleaned; entries never expire (bounded by crashes). Out of scope: recover_backups moves back before unhiding (window where original is back but hidden); pid reuse can drop an entry.

## Not verified (user to check, also in README 확인할 것)

- Explorer right-click verb with the installed app (incl. more than 15 files); installer compile in CI.
- Narrator reading the sidebar number fields (names set through MSAA).
- Real iPhone 15/16 HEIC samples (testdata/private/iphone).
- Explorer sort order against a real Explorer window.
- A real upload site accepting results; implementer-chosen noun-style strings.
- Drag-and-drop smoke; toast-expiry recycle of replaced originals; Windows shutdown during a replace save.
- Performance: 10x12MP JPEG 3.78 s / 604 MB (target 10 s / 600 MB), WebP 7.31 s / 686 MB.
