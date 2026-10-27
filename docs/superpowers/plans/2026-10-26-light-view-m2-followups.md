# M2 follow-ups and rulings

This is the controller ledger from building M2 with subagents. It lists every ruling made on your behalf (R1-R4 in the plan, R5-R15 here), deferred minor findings, and items carried to M3/M5. Use it when planning M3-M5.

# SDD ledger — plan: docs/superpowers/plans/2026-10-26-light-view-m2-heic-worker.md

Spec: docs/superpowers/specs/2026-10-26-light-view-design.md
Branch: m2-heic-worker (from main 606261e)

## Preflight scan

| Pair / task | Produces → consumes | Finding |
|---|---|---|
| T1 → T4 | decode_heif, HeifImage, ColorInfo, HeifError | consistent |
| T2 → T4 | to_srgb_in_place, ColorSource, premultiply_to_bgra | consistent; T4 maps ColorInfo→ColorSource |
| T2 → T5/T6 | Frame.bgra, DecodedImage w/o icc | consistent (T6 builds DecodedImage from worker bgra) |
| T3 → T4/T5 | Request/Response/FailKind/IpcError, read_/write_ fns | consistent |
| T4 → T5 | lv-worker.exe, --test-mode (debug only) | T5 tests run in debug profile → consistent |
| T5 → T6 | WorkerPool::start/decode_heif → DecodeError | consistent |
| T1 → T5/T6 | DLLs copied into target dirs by setup-deps.ps1 | T5 tests need DLLs next to lv-worker.exe → script must copy for debug too (it does) |
| T1 self | Step 5 "cargo tree -p lv-heif ... no x265" is meaningless (x265 isn't a crate) | → R5 |
| T2 self | removes `icc` field; test constructors in viewer.rs/prefetch.rs use it | implementer updates them (in scope) |
| T3–T7 self | | consistent |
| Global vs T5 | Job memory limit max(max_pixels*4*3, 256MiB) | consistent |

## Rulings
- Ruling R5 (T1): replace the "cargo tree has no x265" check with the setup script's installed-tree check plus `vcpkg list` output in the report — x265 is a vcpkg package, not a crate — cost if wrong: none.

## Progress
- Ruling R6 (T1): vcpkg.json uses only default-features:false (port at baseline has no "libde265" feature; libde265 is a hard dependency) — vcpkg rejects the plan's feature list — cost if wrong: none (x265 absent, verified).
- Ruling R7 (T1): accept .vcpkg\installed junction -> vcpkg_installed for vcpkg crate 0.2.15; DLLs copied to target\debug and target\release only (tests use libheif-sys OUT_DIR on PATH) — cost if wrong: T5 tests may need DLLs beside lv-worker.exe (they are copied there).
Task 1: ⚠️ resolved: cargo tree -p lv-app has 0 libheif hits (controller checked).
Task 1: minor (deferred): TooLarge reports display dims when only ispe exceeds (untested); unchecked stride*(h-1)/w*h*4 relies on libheif limit; max_total_memory 12N tight for rotated alpha (measure in T7); junction deletion hazard with Remove-Item -Recurse; HeifError lacks Display/Error; LGPL text not next to testdata; CMake VCPKG_LOCK_FIND_PACKAGE_JPEG warning noise
Task 1: fix round 1/5 (2 addressed — in-repo vcpkg caches, force=true env; commits 7ee6c92..4649574)
Task 1: note for user: %LOCALAPPDATA%\vcpkg\archives (20MB) left from first run, not deleted.
Task 1: complete (commits 606261e..4649574, review clean)
Task 2: minor (deferred): rotated stills peak ~1B/px higher; gray ICC treated as sRGB (comment); qcms release asserts residual (contained by catch_unwind / R4)
- Ruling R8 (T2 minors → T6): carry into Task 6: nclx transfer 1/6/14/15 use the sRGB curve with real primaries (BT.709 inverse-OETF lifts shadows visibly; matches Windows Photos best); measure qcms cost on a sRGB-tagged 12MP JPEG and add a well-known-sRGB-profile fast path if noticeable — cost if wrong: oddly-tagged HEIF/AVIF-style files slightly off vs. strict H.273.
Task 4 checklist (from T2): worker stderr must not be the pixel pipe (color.rs eprintln); qcms panic in worker → caught (catch_unwind) or R4 crash path.
Task 2: complete (commits 4649574..6814578, review clean)
- Ruling R9 (T3): read_request/read_response take a caller cap (min(cap, MAX_PAYLOAD)), try_reserve_exact + read_exact; clean EOF → Ok(None); short fixed-field payload → Malformed — reviewer showed a hostile worker can force 1 GiB allocations; T4/T5 need clean-EOF/pipe-death distinction — cost if wrong: API churn only.
Task 3: minor (deferred): small unbuffered writes (doc note added in fix)
Task 3: fix round 1/5 (3 addressed per R9; commits 740a77d..5cb569d)
Task 3: minor (deferred): resize(n,0) zero-fills/commits pages before peer sends data; no per-kind cap
- Ruling R10 (T4/T5): worker gets `--max-pixels N` on its command line; its request read cap = max(N*4 + 1 MiB, 64 MiB) + 64 (same rule as lv-core max_file_bytes); app's response read cap = N*4 + 64 KiB — callers must never pass MAX_PAYLOAD — cost if wrong: none.
Task 3: complete (commits 6814578..5cb569d, review clean)
Task 4: ⚠️ resolved: (a) libde265 transitive search — app dir is searched first; T5 spawns with cwd = worker dir and a minimal environment (no PATH/RUST_BACKTRACE) so fall-through can't reach attacker dirs (carried to T5); (b) C stdout writes would fail safe (bad magic → crash path); (c) mitigations runtime-confirmed in report.
Task 4: minor (deferred): custom panic hook to avoid backtrace symbolization via inherited RUST_BACKTRACE (T5 clears env); hang mode keeps data alive (debug only)
Task 4: complete (commits 5cb569d..f3cf300, review clean)
- Ruling R11 (T5): start failure = worker dies/hangs/protocol-errors before its first valid response (option A); after a start failure the slot respawns without best-effort mitigations; AttributeList Drop-before-init UB pulled into the fix loop (unsound unsafe) — cost if wrong: a hostile file crashing 3 fresh workers in a row disables that slot until restart.
Task 5: minor (deferred): concurrent-spawn inheritance safe via HANDLE_LIST but no spawn mutex (constraint: no inherit-all CreateProcess elsewhere in app); no-mitigation retry on any CreateProcess error; oversized (>1GiB) request costs a full timeout (check len up front); no deny-only Administrators SID if run elevated; KILL_ON_JOB_CLOSE only checked by query; first_args test hook in prod struct; CreateProcess hang unbounded; 1 MiB pipe buffers (T7)
Task 6 carry: create pool at startup size 2 (R2); remove #[allow(dead_code)] on mod worker.
Task 5: fix round 1/5 (3 addressed — start failures per R11, bounded stderr, AttributeList soundness; commits 676342d..c5a7ff8)
- Ruling R11a (T5, amends R11): degrade only on deaths before the request was delivered (process already exited / write fails); post-delivery deaths count toward 3-strike but never degrade — re-review showed a hostile HEIC could switch a slot to the weaker sandbox — cost if wrong: a mitigation that kills workers only after reading a request (none known) wouldn't trigger the degraded retry.
Task 5: minor (deferred): bidi/format chars not escaped in worker log; no log rate limit; 3 crashing files per slot disable HEIC for the session (accepted nuisance DoS)
Task 5: fix round 2/5 (1 addressed per R11a — degrade only on pre-delivery deaths via stdin monitor/PeekNamedPipe; commits c5a7ff8..ab7131d)
Task 5: minor (deferred → T6 carry): SetHandleInformation failure leaks hProcess/hThread + suspended orphan (wrap first); hang test lacks no-degrade assertion; WorkerPool::decode_heif must reject data > worker request cap before sending (else degrade) — T6 enforces max_file_bytes and pool checks too
Task 5: complete (commits f3cf300..ab7131d, review clean)
Task 6: minor (deferred): NOTICES should say LGPL-3.0-or-later; RoutingDecoder reopens non-HEIF files (add decode_image(ImageFile)); PeekMessageW re-entrancy SAFETY comment; malformed WM_COPYDATA now makes sender open own window (untested); is_srgb_profile ignores wtpt (comment re: absolute intent)
Task 6: complete (commits ab7131d..764db06, review clean)
- Ruling R12 (T7): testdata/private is absent; T7 still adds the #[ignore] real-sample harness (skips when folder missing), measures with the largest redistributable HEIC it can obtain (download for measurement only, not committed, source noted), and lists private-sample checks as pending for the user — cost if wrong: real-device issues surface after merge.
Task 7: note: HEIC first-present 1190/1362/1543ms vs 400ms target (non-grid 12MP sample, single-threaded HEVC decode 680-1045ms); iPhone grid HEICs may parallelize via libheif tile threads — verify with user samples; JPEG 431/525/578ms; memory ~300-350MB app+workers.
Task 7: minor (deferred): README prose dropped "JPEG/memory also miss targets → M5" sentence; README ranges rounded vs report; R3 decision rationale not in README; real_samples follows symlink loops (use file_type); unreadable subdir skipped silently; sample not strictly redistributable
Task 7: complete (commits 764db06..7c62389, review clean)
Final review: With fixes. Important: #1 caps vs MAX_PAYLOAD (≈1GiB hostile file degrades slot at default N=2^28; latent response > MAX_PAYLOAD), #2 libheif max_memory_block_size 256MiB rejects >~67-89MP, #3 README overstates sandbox (reads/network not blocked under R1), #4 missing-DLL system error dialogs (no SetErrorMode), #5 NOTICES LGPL-3.0-or-later.
- Ruling R13 (final): one fix wave for #1-#5 plus cheap high-value items: LIBHEIF_PLUGIN_PATH removed at worker start; libheif codec threads + max decoding threads = clamp(parallelism/2,1,4) and re-measure; terminate pool jobs before prefetch join (exit hang); checked stride*(h-1); README VC++ runtime + restored "JPEG/memory also miss → M5" sentence. Deferred to M3 plan: generic pool request API, per-kind caps/deadlines, "accepted" handshake, validate transupp output in app, forbid libheif jpeg feature not libjpeg-turbo. Deferred to M5: AppContainer, sibling-worker isolation, deny-only Admin SID, pool start off UI thread, per-worker libheif instance reuse — cost if wrong: perf/robustness items surface later.
- Ruling R14 (final fix): Job memory floor max(12N, 256MiB + 4N) accepted (libheif block floor 256MiB + worker 4N copy must fit; identical to old rule for N ≥ ~22.4M) — cost if wrong: none.
Final fix wave: re-review all addressed (commits 7c62389..3c50585).
- Ruling R15 (final residuals, parked): lv-heif/lv-ipc memory doc comments overstate "never Job-killed" under parallel tile decode; stale routing.rs decode() cap comment; 10-bit near-limit HEIC may fail Corrupt via 16-bit intermediate (~200MP+); README worker memory row measured single-threaded — re-measure on AC power with user samples — cost if wrong: misleading comments, rare huge-HEIC failures.

# M3 status of the items deferred to M3

Checked against the M3 branch (`m3-file-ops`) in Task 9. The history above is kept as it was.

| M2 item (R13 deferrals) | Status | Note |
|---|---|---|
| Generic pool request API | **M3에서 해결** | The worker pool takes JPEG-rotation requests as well as HEIC decodes, with per-kind time limits (decode 10 s; JPEG rotate 10 s + 10 s per 100 MP). |
| Validate transupp output in the app | **M3에서 해결** | The app re-checks the worker's JPEG with `jpeg_check` (structure, expected size) and decodes it again before writing; on failure the original is untouched. |
| Allow libjpeg-turbo, forbid libheif's `jpeg` feature instead | **M3에서 해결** | `setup-deps.ps1` allows libjpeg-turbo and fails if libheif was built with `jpeg`. |

Still open for M5 (unchanged): AppContainer, sibling-worker isolation, deny-only Administrators SID, per-kind caps and the "accepted" handshake beyond the time limits above.
