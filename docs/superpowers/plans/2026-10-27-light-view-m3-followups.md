# M3 follow-ups and rulings

Controller ledger from building M3 with subagents: every ruling made on your behalf (R1-R4 in the plan, R5-R22 here), deferred minors, and items carried to M4/M5.

# SDD ledger — plan: docs/superpowers/plans/2026-10-27-light-view-m3-file-ops.md

Spec: docs/superpowers/specs/2026-10-26-light-view-design.md
Branch: m3-file-ops (from main dfc8ade). User asleep; authorized to run all remaining milestones end to end with reviews, merging each locally; no publishing.

## Preflight scan

| Pair / task | Produces → consumes | Finding |
|---|---|---|
| T1 → T6/T7/T8 | FileIdentity, capture, verify, to_extended_path | consistent |
| T2 → T6 | pool request, Request::RotateJpeg, FailKind::NotPerfect | T6 abstracts via RotateService trait → consistent |
| T3 → T6 | inspect, is_mcu_aligned, compose, after_pixel_rotation, set_orientation | consistent |
| T4 → T6 | read_rotation, set_rotation, normalize_exif_orientation | consistent |
| T5 → T6/T7 | Dialog, Toast, layouts | consistent |
| T6 → T7 | before_leave / save prompt before delete | T7 runs after T6 → consistent |
| T7 → T8 | hidden pending-delete paths in rescan | consistent |
| T2 self | four_quarter_turns test may not be byte-identical with TurboJPEG (Huffman table re-opt?) | brief already allows comparing SOS data + markers → R5 |
| T3 self | compose for mirror orientations needs a defined rule | brief asks to define + table-test → fine |
| T6 self | HEIF verify via worker decode | needs pool in save → RotateService trait should also expose decode_heif verify; implementer decides → R6 |
| others | | consistent |

## Rulings
- Ruling R5 (T2): TurboJPEG may re-emit Huffman tables/markers identically or not; "restore bytes" is satisfied if the SOS entropy-coded data and the full marker list (types + payloads) match after 4 turns — cost if wrong: weaker test.
- Ruling R6 (T6): the service trait used by save.rs covers both `rotate_jpeg` and `decode_heif_dims` (for HEIF verification) — cost if wrong: none.

## Progress
Task 1: minor (deferred): absolute() failure fallback prefixes relative path; capture errors map to Missing (callers must not treat Missing as "gone" — carry to T7/T8; consider FILE_READ_ATTRIBUTES open); last_os_error after windows-rs error; 64-bit file id only (ReFS 128-bit); test gaps (mtime-only change w/ index 0, \.\ paths)
Carry T6/T7: re-verify immediately before destructive op; re-capture identity after own ReplaceFileW save.
Task 1: complete (commits dfc8ade..df3cedf, review clean)
- Ruling R7 (T2): JPEGs with data after EOI or an MPF APP2 (motion photos, gain maps, MPF) are never pixel-rotated — refused app- and worker-side with a distinct error; Task 6 falls back to the EXIF-tag path (bytes intact) — reviewer showed tj3Transform drops everything after EOI → data loss — cost if wrong: such photos get tag-only rotation (still lossless).
Task 2: minor (deferred → T6 carry): map rotation Corrupt (e.g. STOPONWARNING on extraneous bytes) to EXIF-only fallback; TooLarge 0x0 dual cause (use rotate_jpeg only); MAXMEMORY margin near budget; rotate_deadline uses DEFAULT_TIMEOUT and acquire waits 10s (long rotation can starve HEIC prefetch); missing turbojpeg DLL disables HEIC (delay-load later); BSD texts only linked (M5 packaging); worker.rs ~870 lines (split when jpeg_dimensions replaced by jpeg_check in T3); test gaps (NotPerfect to decode, arithmetic, Exif-only no JFIF — check TurboJPEG doesn't add APP0); 1s wait even for answered workers
Task 2: fix round 1/5 (1 addressed per R7 — jpeg_extent::check_single_image app+worker refusal; commits cd0e686..4ad37f7)
Task 2: note: HasTrailingData will be common (zero padding after EOI too) → T6 EXIF-only fallback path must be first-class; broken-structure JPEGs now report HasTrailingData not Corrupt.
Task 2: complete (commits df3cedf..4ad37f7, review clean)
Task 3: carry T6: after_pixel_rotation sets Orientation=1; for mirrored compose result (o'=2) save must also set_orientation(..,2); compose_tag_only exists for tag-only path.
- Ruling R8 (T3): after_pixel_rotation takes the target orientation o' (from compose) instead of always 1; set_orientation/after_pixel_rotation verify via read_orientation readback — reviewer showed mirrored files and malformed EXIF could silently lose the user's rotation — cost if wrong: none.
Task 3: minor (deferred): MCU derivation differs from lv-jpegtran for exotic sampling (predicates agree; document); inspect-fail path skips TooLarge label (error label only); new APP1 lands between JFIF and JFXX APP0; grown APP1 makes MPF first-image size stale (tag-only path, MPF files w/o Orientation — document); hostile test coverage of in-range offsets/self-pointer/exact 65533 fit; worker.rs 609 lines; compose_tag_only extra API; no RED evidence
Task 3: fix round 1/5 (2 addressed per R8; commits 658ea4b..474dc0c)
Task 3: minor (deferred): after_pixel_rotation read-back not directly hit by malformed fixture
Task 3: complete (commits 4ad37f7..474dc0c, review clean)
- Ruling R9 (T4): irot angle CCW; new = (old + 4 - user_cw) mod 4; mirrored (imir) primary may be refused → Unsupported; irot→0 keeps the property (angle 0); add irot at end of ipco + ipma assoc essential; iloc construction_method 0 extents after meta shifted by Δ (base_offset adjusted when per-item); self-check extents identical after edit — cost if wrong: mirrored HEIFs can't be rotation-saved.
- Ruling R10 (T4): HEIFs with an alpha aux item are refused for rotation save (libheif rotates color but not alpha; adding irot to alpha risks double rotation in other decoders) → Unsupported; also refused: mirrored, size-changing edits with unknown top-level boxes, multiple ipma — cost if wrong: those rare files can't be rotation-saved.
- Ruling R11 (T4): adding irot also associates it with the primary's thmb and non-alpha auxl items (depth/gain) — matches iPhone shared-irot layout, keeps HDR gain maps aligned; refuse if those items have their own irot/imir; in-place refused if irot index is on dimg tiles; refuse dref≠0/method>2 on size-changing edits, transform after irot, multiple irots — cost if wrong: some unusual HEIFs refused for rotation save.
Task 4: minor (deferred): normalize_exif_orientation Err on malformed Exif must not block irot save (T6 caller); heif_edit.rs ~860 lines (split parse/edit); no RED run
Task 4: fix round 1/5 (4 addressed per R11; commits 5e89633..db3527d)
Task 4: minor (deferred): in-place path ignores followers with their own separate irot (out of step) / shared irot on unrelated non-tile items; thumbnail-owned alpha not rotated; dead imir term; heif_edit.rs ~1040 lines (split parse/edit in M5 cleanup)
Task 4: complete (commits 474dc0c..db3527d, review clean)
- Ruling R12 (T5): one toast at a time (6s); dialog blocks all other input; Enter always = default button; WM_CLOSE with dialog open is cancelled; scrim/card/toast tokens per brief with contrast-tested toast colors; screen-reader names for overlay deferred to M4 — cost if wrong: visual tweaks.
- Ruling R13 (T5): on the permanent-delete dialog the default button (Enter, initial focus) is [취소], not [삭제] — irreversible action must not be one keypress away — cost if wrong: one extra keypress.
- Ruling R14 (T5): forwarded opens (WM_COPYDATA) are deferred while a dialog is open and replayed after it closes; dialogs carry a caller token/target so answers apply to the bound file; Task 8 refresh gates on "dialog open" — reviewer showed answers could apply to the wrong photo — cost if wrong: a forwarded open waits until the dialog closes.
- Ruling R15 (T5): dark toast uses surface2 + ink + hairline (contrast-tested) instead of a literal inversion — cost if wrong: visual only.
Task 5: minor (deferred): mouse-leave tracking swallowed while dialog open; show_dialog/show_toast silent on borrow failure; toast timer KillTimer before borrow; delete title reserves 2 lines; toast may cover floating toolbar in narrow windows; overlay.rs 728 lines; test gaps (toast not hit under dialog, min width layout, ink2 detail contrast)
Task 5: fix round 1/5 (R13 default cancel, R14 deferral+target, R15 comment; commits b7f8e75..4f7c0e9)
Task 5: carry T6: deferred-open replay after a dialog answer must not run when the answer closes the window (check closing/destroy); SaveRotation prompt hook must live inside commands::open so replay/WM_COPYDATA go through it; Overlay.dialog pub field + finish() target take fragility.
Task 5: complete (commits db3527d..4f7c0e9, review clean)
- Ruling R16 (T6): JPEG ladder: no single image/MPF → tag-only; p==0 or unaligned → tag-only; worker NotPerfect/HasTrailingData/Corrupt → tag-only; other worker errors → Failed (no write). HEIF normalize_exif Err → Unsupported (no write). Atomic write re-verifies identity before temp and before ReplaceFileW, restores times, re-captures identity. Prompt hook inside commands::open so all entry paths are covered; close-after-answer skips deferred replay; read-only original → toast, no write — cost if wrong: more tag-only saves than strictly needed.
- Ruling R17 (T6): hung/crashed worker during rotation → Failed (no write), via a dedicated PoolError variant (not tag fallback) — safer than R16's literal text — cost if wrong: user retries instead of getting a tag save.
Task 6: minor (deferred): ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 may orphan original under internal name; long-name temp create fails generically; deferred forwarded path dropped on failed save-then-close / identity None; verify→ReplaceFileW gap undocumented; commands.rs ~575 lines (move save orchestration to module)
Task 6: fix round 1/5 (6 addressed + R17; commits de399a7..4a90de9)
Task 6: minor (MUST-FIX in final wave): 1176/1177 with target present still removes temp (only copy if third party created target in gap) — always try non-replacing MoveFileExW then KeptCopy. Minor: temp_pid accepts leading zeros (compare exact name); unhide failure in KeptCopy leaves copy sweep-eligible; WM_ENDSESSION discards SaveDone; sweep reads folder per save
Task 6: complete (commits 4f7c0e9..4a90de9, review clean)
- Ruling R18 (T7): recycle bin only for DRIVE_FIXED + SHQueryRecycleBinW ok, else permanent-confirm; commit via IFileOperation on a dedicated STA thread after identity re-verify (Changed/Missing → don't commit); never DeleteFileW for recycle-intended; directories/reparse points refused; delete blocked during a save of that path; unsaved rotation prompt first — cost if wrong: some local drives ask for permanent-delete confirmation.
Task 7: note for user: Recycle Bin contains test items lv-m3-smoke-23080-a.jpg, lv-m3-smoke-23080-b.jpg, lv-m3-recycle-test-{18260,26968,35816}.txt (safe to empty).
Task 7: minor (deferred): bin disabled/size 0 → every delete fails visibly at commit (should detect and offer permanent dialog); SHQueryRecycleBinW on UI thread per delete; opening a queued-not-pending file re-hides it; file made read-only while pending still recycled; exit wait may expire mid-operation; Ctrl+Z after permanent delete restores older pending; delete.rs/commands.rs ~630 lines; online-only cloud placeholders may be refused as reparse points (unverified); late shell fallback after PreDeleteItem untested
Task 7: fix round 1/5 (UTF-16 length + extras; commits 7258769..ee7189d)
Task 7: complete (commits 4a90de9..ee7189d, review clean)
- Ruling R19 (T8): rescans deferred while a dialog is open or a save of the current photo runs; confirmed-absent current photo → neighbour (+toast "보던 사진이 폴더에서 사라졌어요" if rotation was unsaved); modified externally → invalidate+re-decode, reset rotation, toast "사진이 바깥에서 바뀌어 다시 불러왔어요"; single Missing never removes; overflow → full rescan; unsupported locations run without watch — cost if wrong: new toast texts / UX tweaks.
- Ruling R20 (T8, amends R19): silent reload when the current photo changes externally with no pending rotation; toast only when an unsaved rotation was discarded — less noise — cost if wrong: user isn't told a photo was reloaded.
Task 8: minor (deferred): change during in-flight decode not re-checked (stale pixels until next change; saves refuse safely); deleted+recreated folder never re-watched; open dir handle may block USB safe-eject; wiring tests overlay_changed/on_save_done → rescan missing; watch_reports_new_file subfolder assertion timing-based; viewer.rs ~844 lines (move rescan to module)
Task 8: fix round 1/5 (off-UI rescan, detach drop, panic leak guard; commits b03c4ec..92cb75b)
Task 8: minor (deferred): discarded rescan result retries immediately (skips stretched debounce) while browsing a slow NAS; lost notice if App borrowed in on_changed/on_rescan_done (theoretical); viewer.rs ~900 lines
Task 8: complete (commits ee7189d..92cb75b, review clean)
Task 9: note for user: Recycle Bin has del2.jpg, del3.jpg (from lv-m3-smoke) — safe to empty.
Task 9: minor (deferred): README should say JPEG bytes/size may change per save (Huffman re-optimized) though pixels identical; "260자" wording; M2 follow-up rows lack commit hashes
Task 9: complete (commits 92cb75b..e22991f, review clean)
Final review: With fixes. Critical: after_replace_error can delete the only copy. Important: kept copy sweep-eligible if unhide fails; compromised worker can replace JPEG content on rotation; OneDrive cloud files may be refused as reparse points.
- Ruling R21 (final): one fix wave: (1) never delete the temp unless the original verifies Same; else MoveFileExW (non-replacing) if target absent else KeptCopy; (2) kept copies renamed out of the temp pattern + process-wide kept set excluded from sweep; (3) rotations run in a fresh single-use worker (not the shared pool); (4) allow IO_REPARSE_TAG_CLOUD* (and dedup/WOF) reparse tags for delete, still refuse name surrogates; (5) spec §8 amended: "DCT coefficients identical / pixel-exact decode after four turns" (TJXOPT_OPTIMIZE kept), README wording, pixel-exact 4-turn test; (6) TooLarge rotation refusal → tag-only; (10) SHChangeNotify(SHCNE_UPDATEITEM) after save; (12) log HEIF refusal reasons; (13) exact temp name match, FindFirstFileW pattern sweep. Deferred to M5: handle-held verify→replace gap, handle-based permanent delete, surfacing non-recycled deletes, rescan/save epoch, SHQueryRecycleBinW off UI thread, padding-only trailer pixel rotation, USB safe-remove (M4) — cost if wrong: listed residual risks remain until M4/M5.
Final fix wave: re-review all addressed (commits e22991f..78468d0).
- Ruling R22 (final residuals, parked → M5): KEPT set compares exact PathBufs (case/UNC spelling) and is process-scoped (restart gap) — use same_path/file ID + retry unhide; misleading verify after ReplaceFileW error may produce extra copy or wrong KeptCopy toast; is_refusal substring match; reparse_tag open failure shows NotAFile; fresh-worker PID test could flake on PID reuse (compare job handles); HEIF save verify still uses pool worker dims (edit itself is in-app Rust) — cost if wrong: extremely unlikely only-copy loss chains (4 coincident failures), cosmetic messages.

# M4 status of the items carried to M4

Checked against the M4 branch (`m4-info-settings`) in Task 8. The history above is kept as it was.

| M3 item | Status | Note |
|---|---|---|
| R12 (T5): screen-reader names for the overlay deferred to M4 | **M4에서 해결** (83aeb77) | Dialogs are a Group named by the title with Text, a CheckBox (Toggle) and Buttons, focus on the dialog's focused item; the toast is Text with its [실행 취소] Button and is announced with a UIA notification event. |
| R21 deferred list: USB safe-remove (M4) | **M4에서 해결** (54d1895, 6cf4aed) | The folder watch registers its directory handle for `WM_DEVICECHANGE`. On query-remove it closes the handle (keeping the notification until remove-complete or query-remove-failed, per Microsoft's sample, ruling R-T7a); a failed removal restarts the watch; after removal the same folder is watched again on the next open (R-T7b). Real-drive check is on the README user-check list. |
| Task 8 minor: open dir handle may block USB safe-eject | **M4에서 해결** (54d1895) | Same change as the row above. A rescan thread listing the folder at the moment of removal can still make one removal fail (user-check item). |
| Task 8 minor: deleted+recreated folder never re-watched | 일부 해결 (6cf4aed) | Only for a removed drive: following the same folder again after a removal starts a new watch. A folder deleted and recreated on a fixed drive is still not re-watched until the viewer moves to another folder. |

# M5 status of the items carried to M5

Checked against the M5 branch (`m5-harden-ship`) in Task 9. The history above is kept as it was. Items not listed here are still open.

| M3 item | Status | Note |
|---|---|---|
| R22: KEPT set compares exact PathBufs (case/UNC spelling) | **M5에서 해결** (9bfa5f8) | Kept copies are known by file ID, so another spelling of the same path is still excluded from the sweep. The set is still process-scoped (a copy kept by a crashed earlier run is treated as a stale temp file), unchanged by design. |
| Task 2 minor: BSD license texts only linked (M5 packaging) | **M5에서 해결** (bfcf3da, 41737b7) | `THIRD_PARTY_NOTICES.md` (made by `scripts/package.ps1`) carries the full libjpeg-turbo, IJG, libspng, libpng and zlib texts and ships beside `light-view.exe`. |
| Oversized files: viewer.rs, delete.rs, commands.rs, save.rs, overlay.rs, heif_edit.rs | **M5에서 해결** (8a9d70e, d786eee, 95272fc, a14c129, e87ea00, 903254b) | Split by responsibility with no behaviour change. |
| R21 deferred: handle-held verify→replace gap, handle-based permanent delete, surfacing non-recycled deletes, rescan/save epoch, `SHQueryRecycleBinW` off the UI thread, padding-only trailer pixel rotation | 미해결 | Not in the M5 plan. |
| R22 rest: misleading verify after a ReplaceFileW error, `is_refusal` substring match, reparse-tag open failure message, fresh-worker PID test | 미해결 | Not in the M5 plan. |
