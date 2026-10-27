# M1 follow-ups and rulings

This is the controller ledger from building M1 with subagents. It lists every ruling made on your behalf (R1-R18), the minor findings deferred to later milestones, and the visual checks only a person can do. Use it when planning M2-M5.

# SDD ledger — plan: docs/superpowers/plans/2026-10-26-light-view-m1-core-viewer.md

Spec: docs/superpowers/specs/2026-10-26-light-view-design.md
Branch: m1-core-viewer (from main 46936e4)

## Preflight scan

| Pair / task | Produces → consumes | Finding |
|---|---|---|
| T1 → T2 | is_supported_extension | consistent |
| T1 → T3 | sniff_format, ImageFormat, SNIFF_LEN | consistent; T3 HEIF test bytes sniff as Heif (major heic) |
| T3 → T6 | DecodedImage, DecodeError, DecodeLimits, decode_file | consistent |
| T4 → T10 | Viewport API | consistent |
| T5 → T10 | Command, command_for_key/wheel | consistent |
| T6 → T10 | Prefetcher, Decode, FileDecoder | consistent (T10 injects Prefetcher) |
| T7 → T8/T9/T11 | Settings fields | consistent |
| T8 → T9 | Renderer drawing methods | T8 lists `fill_round_rect(...)` without signature → R3 |
| T8 → T11 | window class name | T11 needs class `goodflow.LightView.Window`, T8 doesn't set it → R4 |
| T9 → T10 | layout, hit_test, title format | consistent ("name · i / n") |
| T1 self | tests vs code | consistent |
| T2 self | natural_cmp needs unsafe FFI vs `forbid(unsafe_code)` | conflict → R1 |
| T3 self | header-only PNG may fail header parse in png crate before size check | risk → R2 |
| T4 self | tests vs rules | consistent |
| T5–T7 self | | consistent |
| T8–T12 self | | consistent |

## Rulings

- Ruling R1: lv-core uses `#![deny(unsafe_code)]` (not forbid) with `#[allow(unsafe_code)]` only on the Windows `natural_cmp` impl — forbid cannot be overridden by an inner allow, plan's own exception requires it — cost if wrong: none; slightly weaker lint.
- Ruling R2: T3 oversized-PNG fixture may be a structurally valid PNG (IHDR declaring 100000×100000 + tiny IDAT + IEND); requirement is TooLarge returned before pixel allocation — header-only file could be rejected by the png crate as Corrupt for an unrelated reason — cost if wrong: none.
- Ruling R3: T9 may add drawing methods to Renderer (render.rs) as needed (rounded rect fill/stroke, glyph text, shadow); T8 only needs what Task 8 draws — plan left signature open — cost if wrong: minor churn.
- Ruling R4: T8 registers the window class as `goodflow.LightView.Window` (spec §9 identity, T11 depends on it) — cost if wrong: rename.

## Progress
Task 1: minor (deferred): BM sniff is weak (plan-driven); ftyp brands ignore box size; compat Vec alloc; few HEIF brand tests
Task 1: complete (commits 46936e4..39b3e90, review clean)
Task 2: minor (deferred): refresh drops unsupported-ext current (pass keep); same_path re-lowercases; StrCmpLogicalW total-order/tie-breaker; read_dir entry error aborts listing, symlinked files excluded; test gaps (unsupported current kept, case-insensitive match, neighbor OOR, empty nav); non-windows fallback never compiled
Task 2: fix round 1/5 (1 addressed, 0 open — scan relative/forward-slash dup; commits 2e57d50..484c473)
Task 2: minor (deferred): cwd-mutating test may race with future cwd tests
Task 2: complete (commits 39b3e90..484c473, review clean)
Task 2: minor (deferred): listing.rs not cargo-fmt clean
Task 3: minor (deferred): ImageError::Limits maps to Corrupt not TooLarge (user sees wrong message); only orientation 6 tested end-to-end (fixture can't distinguish 5/6, 7/8) — add asymmetric 1-8 test; double sniff in decode_still; decode_file reads whole file with no size cap; peak memory ~1.8GB at 256MP limit; no WebP/BMP decode tests
Task 3: fix round 1/5 (1 addressed, 1 open — frame budget test doesn't exercise budget; commits bf273ce..2954f56)
Task 3: fix round 2/5 (1 addressed, 0 open — budget now proven by direct test + mutation check; commits 2954f56..6ef0cfe)
Task 3: minor (deferred): no end-to-end test that decode_bytes wires max_pixels*4 as animation budget; animated WebP EXIF lookup untested end-to-end (no encoder)
Task 3: complete (commits 484c473..6ef0cfe, review clean)
- Ruling R5 (T4): fit = min(vw/iw, vh/ih); initial = min(fit,1); min scale = min(fit, 0.05); max = max(32, fit); "fitted" = |scale-fit|<1e-4; resize keeps view-centre point when not fitted; degenerate sizes clamp to >=1 — brief left these open — cost if wrong: small UX tweak.
Task 4: minor (deferred): no tests for non-fitted resize / degenerate sizes / small-image toggle; NaN factor/anchor not guarded; absolute FIT_EPS; scale may sit below new min after resize
Task 4: complete (commits 6ef0cfe..b2fbc6e, review clean)
- Ruling R6 (T5): unmodified keys map only without Ctrl; Shift ignored for +/-; Ctrl combos require Ctrl; any Alt -> None — brief silent on extra modifiers — cost if wrong: key remap.
Task 5: minor (deferred): Alt+wheel not filtered (caller in T10 should skip wheel when Alt held); Ctrl+Shift+S/Z map to Save/Undo; delta 0 = down
Task 5: complete (commits b2fbc6e..a47a915, review clean)
- Ruling R7 (T6): request replaces pending queue; cached paths never re-decoded; in-flight results kept only if in latest wanted; on_ready once per insert, outside lock; errors 0 bytes; never evict wanted[0] even over budget; get updates LRU; drop joins workers — brief left details open — cost if wrong: prefetch tuning.
- Ruling R7a (T6, amends R7): eviction = non-wanted by LRU, then wanted (except [0]) highest index first; no on_ready if inserted entry is evicted in same pass — reviewer showed plain LRU evicts the next photo and thrashes — cost if wrong: prefetch tuning.
Task 6: minor (deferred): zero-byte error entries evicted for no gain; on_ready may fire during Drop; Drop waits on in-flight decode; panicking third-party Decode/on_ready kills worker & leaves path in_flight forever (consider catch_unwind in worker_loop); Harness field drop order; no test for drop-then-rerequest-while-in-flight; drop test message on Disconnected
Task 6: fix round 1/5 (1 addressed, 0 open — priority eviction R7a + no on_ready on self-evict; commits f1c1404..636d6cc)
Task 6: complete (commits a47a915..636d6cc, review clean)
- Ruling R8 (T7): backup = settings.toml.bak (overwrite); rename failure still returns defaults; save via settings.toml.tmp + rename; non-NotFound read errors -> defaults without rename — brief silent — cost if wrong: trivial.
Task 7: minor (deferred): invalid-UTF8 settings returns defaults without backup (per R8); .tmp left if rename fails; no tests for parent-dir creation / .bak overwrite / no .tmp leftover
Task 7: complete (commits 636d6cc..0eec0b6, review clean)
- Ruling R9 (T8-T11): subagents cannot see the screen, so plan "manual verification" checklists are collected and handed to the user at M1 end; implementers must still launch the exe (smoke: starts, no panic, exits on WM_CLOSE) and verify what they can programmatically — cost if wrong: visual bugs found later by user.
- Ruling R10 (T8 fix): move placement code to lv-app/src/placement.rs now (window.rs 574 lines, T9 adds to it) — cost if wrong: none.
Task 8: minor (deferred): one large unsafe block in run() wraps safe code; GetMessageW -1 treated as true; placement not saved on WM_ENDSESSION; ensure_device failure no retry, rebuild failure swallowed (blank window); renderer retains BGRA copy (drop after upload, re-set on device loss); sync decode before window shown (T10); on-monitor check accepts 1px overlap; release-mode errors silent (MessageBox); /DEPENDENTLOADFLAG:0x800 hardening for static imports; save_placement maximized logic untested; WM_DPICHANGED discards zoom/pan (T10 should preserve)
User-verify (T8): light/dark live switch incl. Win10 title bar repaint; PerMonitorV2 effective at runtime; mixed-DPI restore; 4032x3024 fit / 200x100 at 100%; maximized restore; .txt shows message
Task 8: fix round 1/5 (3 addressed, 0 open — buffer length check, oversized release+TooLarge msg, DPI restore guard; placement.rs split; commits 65e63cd..c2289ec)
Task 8: complete (commits 0eec0b6..c2289ec, review clean)
- Ruling R11 (T9): sizes (44 buttons, Counter 76, Save 72, 4 gap, 6 pad, 12 bottom, pinned 56, info hidden <560); Fluent icons w/ MDL2 fallback; Prev/Next stubbed until T10; title = filename; tooltip strings fixed; 250ms auto-hide timer + TME_LEAVE; optional fade <=150ms honoring reduce motion — plan left visuals open — cost if wrong: visual tweaks.
- Ruling R12 (T9): toolbar accessible names (spec §4.2 UIA) deferred to M4 — needs UIA fragment provider, not in M1 plan — cost if wrong: screen readers see unlabeled toolbar until M4.
- Ruling R11a (T9, amends R11): pinned side texts shown only if measured width fits (560 floor kept); WM_GETMINMAXINFO min width derived from widest layout — reviewer showed clipped slivers & unreachable buttons — cost if wrong: none.
Task 9: minor (deferred): hairline also in light mode (matches mockup); zoom buttons look enabled on undecodable file; window.rs 541 lines (move toolbar App methods out); DirectWrite objects recreated per draw; redundant on_command arm; disabled buttons have normal tooltips; fade not implemented
User-verify (T9): auto-hide/hover; pinned layout & image shrink; info hidden when narrow; dark pill hairline/no shadow; glyphs (rotate-left mirror, pinned icon); counter baseline; caption colour Win11
Task 9: fix round 1/5 (3 addressed, 0 open — dark tooltip, fit-checked info, min width; commits 072a965..9fdca46)
Task 9: minor (deferred): text_fits glue in draw untested; 560 floor effectively dead (min width 620); restored placement narrower than min may bypass clamp; window.rs 584 lines
Task 9: complete (commits c2289ec..9fdca46, review clean)
- Ruling R13 (T10): viewer.rs pure + window glue split to get window.rs <~500; on_ready only PostMessage(WM_APP+1); budget=settings or min(RAM/4,2GiB), 2 workers; scan failure -> single-entry list; window shown before decode, 150ms loading text; Alt+wheel ignored; drag starting on toolbar = click; fullscreen WS_POPUP not saved as placement; dropped dir -> first_supported_in helper in lv-core — plan left these open — cost if wrong: behaviour tweaks.
- Ruling R13a (T10, amends R7a/R13): request() evicts all non-wanted entries immediately; RAM budget stays only as cap — spec §7 300MB target is binding over the plan's budget formula (smoke run 745MB) — cost if wrong: slightly more re-decoding when going back.
Task 10: minor (deferred): toolbar info/zoom show previous photo while pending; rotation dropped while pending (accept once Shown has quarters); zoom/pan while pending wasted; HwndCell not cleared on NCDESTROY; first_supported_in aborts on one bad entry + duplicates filter/sort; WM_DPICHANGED while fullscreen applies suggested rect; wheel remainder not reset on sign flip; folder scan before window shown (NAS delay); no tests for keep-previous/loading_overdue/stale check_current
User-verify (T10): rotation direction; pan never shows background; GIF plays; 1s catch-up after holding →; Save button highlighter while disabled
Task 10: fix round 1/5 (3 addressed per R13a — memory 745->389MB at p100, loading overlay, rotation kept in Shown; commits 6591526..b8b7111)
- Ruling R14 (T10): to approach spec §7 300MB take option (a) drop renderer pixel copy (rebuild from Viewer's image on device loss); not reducing prefetch depth or budget; residual gap re-measured in M5 — cost if wrong: ~340MB steady state for 12MP mid-folder, above the 300MB target.
Task 10: fix round 2/5 (renderer copy dropped per R14; mid-folder 12MP ~350-400MB private; commits b8b7111..56c9972). Re-review of rounds 1-2: all addressed.
Task 10: minor (deferred): going back to the still-shown rotated photo resets quarters (title/unsaved flag wrong; matters for M3); stale loading flag when returning to shown photo after 150ms; no retry InvalidateRect after ensure_device failure; 1/12 smoke runs exited 0 unexpectedly (not caused by diff; investigate close path/smoke script); device-loss re-upload unexercised at runtime
Task 10: complete (commits 9fdca46..56c9972, review clean)
- Ruling R15 (T11): AUMID before settings; forward retries FindWindow 2s then falls back to own window; no-path forward just activates; SendMessageTimeout 2s; receiver validates + opens via existing open path; interior NUL stops decoding — brief left open — cost if wrong: minor.
Task 11: ⚠️ resolved by controller: settings default open_in_new_window=false (derive Default; true only in test data) — forwarding on by default; startup race not a gap (cross-thread SendMessage is only delivered once primary pumps messages, after the initial open).
Task 11: minor (deferred): null-check lparam in WM_COPYDATA; reject non-absolute received paths; hostile UNC path blocks UI (tradeoff); LRESULT(1) even if open skipped; timeout-then-late-processing may open two windows
Task 11: complete (commits 56c9972..6f25285, review clean)
User-verify (T11): Explorer Open-with double-click reuses window & comes to front; real NAS share
- Ruling R16 (T12): --timing prints first-present-ms to stderr + %TEMP%\light-view-timing.log; README Korean-first, marks M3/M4 keys as planned; LICENSE holder goodflow 2026; cargo fmt --all as separate style commit — brief minimal — cost if wrong: doc edits.
Task 12: note: first-present median 516ms vs spec §7 200ms target (i7-1360P, synthetic 12MP JPEG) — recorded in README, deferred to M5 profiling; surface to user.
Task 12: minor (deferred): has_image true with no device could trigger timing early; --timing silent when forwarded to running instance (README note); trailing --timing ignored (documented); unsourced HEIC claim in README
Task 12: complete (commits 6f25285..4c74fe0, review clean)
Final review: With fixes. Important: #1 back-to-shown-photo state (loading/quarters/animation), #2 folder refresh unwired, #3 decode memory not tied to RAM + whole-file read before sniff, #4 WM_COPYDATA null lparam UB, #5 RGBA->BGRA conversion on UI thread per flip.
- Ruling R17 (final): fix #1, #3, #4 now (+ cheap hardening: /DEPENDENTLOADFLAG:0x800, catch_unwind in prefetch worker_loop, brief check of 1/12 exit-0 smoke hypothesis). #2 folder refresh handed to M3 (needs same list-mutation path as delete/undo) — cost: M1 doesn't see external folder changes. #5 decode output -> premultiplied BGRA decided for M2 worker protocol — cost: page flips of 12MP may exceed 16ms in M1.
Final fix wave: re-review all addressed (commits 4c74fe0..7847a8a).
- Ruling R18 (final residuals, parked): WM_CLOSE-posted-then-launch race loses B's path (only when WM_CLOSE is posted, e.g. taskkill/scripts; manual close sends synchronously) — deferred to M2 with fix: in on_copy_data PeekMessage(WM_CLOSE)/closing flag → return 0 so B falls back to Primary — cost if wrong: rare lost double-click. Also parked: decode_file read_to_end unbounded after size check (use take); budget/8 comment ignores file buffer (~1.5x budget worst case); after() try_borrow_mut latent skip; memory_budget_mb=0 refuses all images (clamp); smoke runs write real settings.toml.

# M2 status of the parked M1 items

Checked against the M2 branch (`m2-heic-worker`) in Task 7. The history above is kept as it was; this section only says what moved. Items not listed here are still open.

| M1 item | Status | Note |
|---|---|---|
| R18: WM_CLOSE-posted-then-launch race loses the second launch's path | **M2에서 해결** (764db06) | `on_copy_data` refuses the path while a WM_CLOSE waits in the queue (`PeekMessageW`) or the window is closing (`mark_closing`), so the second launch falls back to being primary. |
| R18: `decode_file` `read_to_end` unbounded after the size check | **M2에서 해결** (764db06) | The read is `take(room + 1)`; a file that grew past the cap is `FileTooLarge`. |
| R18: `memory_budget_mb = 0` refuses every image | **M2에서 해결** (764db06) | `memory_budget` treats 0 like unset (`filter(\|&mb\| mb > 0)`), with a test. |
| Task 1: ftyp brands ignore the box size | **M2에서 해결** (764db06) | Brands are read only inside the `ftyp` box; size 0 means to the end; test `ftyp_brands_limited_to_box`. |
| Final review #5 / R17: RGBA to BGRA conversion on the UI thread per flip | **M2에서 해결** (6814578) | Every decoder returns premultiplied BGRA8 sRGB (`premultiply_to_bgra`); the renderer only checks the length and uploads. HEIF output comes from the worker already converted. |
| R17: `catch_unwind` in the prefetch `worker_loop` | M1에서 이미 해결 | Done in the M1 final fix wave (`prefetch.rs`); the HEIF worker has its own `catch_unwind` around libheif and color conversion. |
| Not an M1 item: first-present target (200 ms JPEG, 400 ms HEIC) | 미해결 | Measured in Task 7 (README "현재 수치"): 12 MP JPEG ~430-580 ms, 12 MP HEIC ~1.2-1.5 s. For HEIC, libheif/libde265 decoding (single-threaded, 0.7-1.0 s) is almost all of it; pipe transfer is ~30 ms and color conversion is negligible. Goes to M5 profiling; see the follow-up note in README. |

Still parked from M1 and untouched by M2: folder refresh (M3), the per-task minor lists above, and the user-verify visual checks.

# M3 status of the parked M1 items

Checked against the M3 branch (`m3-file-ops`) in Task 9. The history above is kept as it was.

| M1 item | Status | Note |
|---|---|---|
| Final review #2 / R17: folder refresh unwired (external changes not seen) | **M3에서 해결** (b03c4ec, 92cb75b) | `ReadDirectoryChangesW` watch with a 200 ms debounce; the rescan runs off the UI thread and calls `FolderList::refresh`; the current photo is kept by path, and a pending delete stays hidden. |

# M4 status of the parked M1 items

Checked against the M4 branch (`m4-info-settings`) in Task 8. The history above is kept as it was.

| M1 item | Status | Note |
|---|---|---|
| R12 (T9): toolbar accessible names (spec §4.2 UIA) deferred to M4 | **M4에서 해결** (83aeb77) | A UI Automation fragment provider exposes the toolbar as a ToolBar with Buttons named by their tooltips, the counter as Text and the pin button with a Toggle state; an auto-hidden toolbar stays in the tree as offscreen. Checked with a UIA client; Narrator itself is on the README user-check list. |

# M5 status of the parked M1 items

Checked against the M5 branch (`m5-harden-ship`) in Task 9. The history above is kept as it was. Items not listed here are still open.

| M1 item | Status | Note |
|---|---|---|
| Task 12 / M2 table: first-present target (200 ms JPEG, 400 ms HEIC) | 일부 해결 (97afd57, 4075e84) | `lv-bench` measures it. 12 MP JPEG first pixel 543 → 375 ms in Task 3 and 286–290 ms in the Task 9 run (AC power): **still over 200 ms**. Most of what is left is Direct2D/D3D device creation (about 250–400 ms on AC), which does not depend on the photo. 12 MP HEIC is not measured (no sample). Recorded in README "현재 수치". |
| R14 (T10): ~340 MB steady state for a 12 MP photo mid-folder, above the 300 MB target | **M5에서 해결** (97afd57) | Prefetch keeps one photo on each side, mips are uploaded one level at a time and freed. `lv-bench` jpeg12mp: 294 MB in the Task 9 run (target 300 MB, small margin). |
| R18: smoke runs write the real settings.toml | 일부 해결 (97afd57, bfcf3da) | `lv-app --bench` starts with default settings and saves nothing; the portable build (`portable.txt`) keeps its settings in `data\` beside the exe. Other app runs (including `--timing`) use the normal settings file. |
