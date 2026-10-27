# M4 follow-ups and rulings

Controller ledger from building M4 with subagents: every ruling made on your behalf (R1-R4 in the plan; R5, R6, R-T7a, R-T7b here), deferred minors, and items carried to M5. User-check items are in README (확인할 것).

# SDD ledger — plan: docs/superpowers/plans/2026-10-27-light-view-m4-info-settings.md

Spec: docs/superpowers/specs/2026-10-26-light-view-design.md
Branch: m4-info-settings. User authorized running remaining milestones end to end with reviews, merging locally; no publishing.

## Preflight scan

| Pair / task | Produces → consumes | Finding |
|---|---|---|
| T1 → T3/T4/T5/T6 | strings::t / S keys | consistent; later tasks add keys |
| T2 → T3 | read_exif, ExifSummary, ExifEntry | T3 reads EXIF in background alongside decode → needs prefetch to carry it (implementer decides where) → R5 |
| T5 → T1 | LangSetting persisted in settings | consistent |
| T6 → T3/T4/T5 | needs control lists from each UI | T6 after T3–T5 → consistent |
| T7 self | WM_DEVICECHANGE handled in window proc | consistent |
| others | | consistent |

## Rulings
- Ruling R5 (T3): EXIF is parsed on the prefetch thread from the same bytes as the decode and carried with the cached result (DecodedImage or entry), never read on the UI thread — cost if wrong: none.

## Progress
Task 1: carry T5: after language change redraw everything and re-apply window title; tests must not call strings::set(En) globally; English permanent-delete title may exceed 2 lines (check layout).
Task 1: minor (deferred → T5 carry): English permanent-delete prompt may wrap to 3 lines (shorten or measure lines); non-string language value resets settings; Content::Message caches translated text (re-derive on language change); "as a tag" wording
Task 1: complete (commits 69c1919..58fa44a, review clean)
Task 2: minor (deferred → T3 carry): exposure 8/10 shows "1/1s" (print decimal when not ~1/N); NIKON CORPORATION + NIKON D90 dedupe; GPS (0,0) treat as absent and normalize -0; oversized test doesn't hit truncation path; redundant clean
Task 2: complete (commits 58fa44a..ac7f151, review clean)
- Ruling R6 (T3): card doesn't block navigation (updates per photo); full list scroll keys; Esc closes list then card; date formatting ko "2026. 10. 3" + "오후 5:42" / en "Oct 3, 2026" + "5:42 PM"; T2 display minors fixed in T3; copy = value only — cost if wrong: UX tweaks.
INCIDENT (T3 smoke): implementer used SendKeys; keystrokes (i, Enter, Down, Ctrl+C, Right) landed in the user's Chrome (a Google One page) — text selected/page scrolled, nothing submitted seen. Reported to user. Rule added to implementer-instructions: only PostMessage to the app HWND, never global input injection.
Task 3: concerns: exif_card.rs ~790 lines; copied values keep display quotes; Enter while card open opens list/copies instead of fullscreen.
Task 3: minor (deferred): card model rebuilt per WM_SETCURSOR; EXIF memory outside prefetch budget (bounded ~4MB worst); exif_card.rs ~790 lines (split draw); HEIF EXIF wiring only consistency-tested; scrollbar decorative only
Task 3: fix round 1/5 (unquoted ASCII, close w/o file, short-window clamp; commits 2faed6d..8c44508)
Task 3: complete (commits ac7f151..8c44508, review clean)
Task 4: concerns: flaky save test once in full run; NOTICES not copied next to exe (M5 packaging); X button inert while About open; unsupported-drop toast in all states
Task 4: minor (deferred): start-screen [열기] not keyboard-focusable (Ctrl+O covers; UIA task); with_com init/teardown per call (init STA once at startup — M5); About blocks X button/forwarded opens; NOTICES copy beside exe (M5 packaging); drop filter by extension only vs sniff; input/commands/window.rs over 600 lines (split M5); menu keeps open-time file state
Task 4: fix round 1/5 (menu key reopen + extras; commits df5cde6..5d267ff)
Task 4: complete (commits 8c44508..5d267ff, review clean)
Task 5: minor (deferred): Open dialog keeps old filter names after language change; busy borrow during WM_PAINT leaves window undrawn; language_switch_redraws checks model only, single-instance not unit-tested; settings_win.rs ~924 lines (split model/draw); Scene::Message allocates per scene(); no hover highlight
- Task 5: fix round 1/5 (field-level saves via update_settings/sync_from, held Space/Enter, capture on press, open-in note, unpin re-arms auto-hide; commits 7d03adf..337a815)
- Task 5: complete (commits 5d267ff..337a815, review clean). Residual minors: no cross-process file lock, shared .tmp name, transient read error can reset other fields, settings_win.rs 973 lines, held-key tests not red first
- Task 6: complete (commits 337a815..83aeb77, review clean, opus). Minor (deferred): get_object after WM_DESTROY recreates host; Err(empty) null out-ptr relies on caller init (windows 0.58 shim); toast undo exposed enabled while dialog open; only root disconnected; stale index-based invoke race; HWND recycle race; settings group focused but not focusable; SelectionContainer without Selection pattern; no-renderer paint skips publish; uia.rs ~690 lines (move glue to uia_glue.rs); EXIF list only drawn rows; no StructureChanged; tree rebuilt per paint once client connected
- Task 7: review 1 Needs fixes — Important: same folder not re-watched after REMOVECOMPLETE (R-T7b). Minor fixed in round: wait-failure path closes handle before unregister; log query-remove while borrowed. Minor (deferred): test flake if another process opens temp folder; watch_loop panic leaks handle; rescan thread can block removal (user-check); watch.rs ~738 lines (split device_watch.rs)
- Flaky test noted: viewer::tests::deleting_hides_the_photo_and_undo_shows_it_again (viewer.rs:1340) failed once in a full run (prefetch timing?) — check in final review
- Task 7: fix round 1/5 (same-folder re-watch after removal, wait-failure keeps handle, busy log; commits 54d1895..6cf4aed)
- Task 7: complete (commits 83aeb77..6cf4aed, review clean)
- Task 8: complete (commits 6cf4aed..b397526; review folded into final whole-branch review). Flaky delete/undo test fixed (a313010, 105/300 -> 0/300). Known: pinned toolbar not shown when photo opened from start screen until mouse moves (commands::after lacks refresh_toolbar); system Open dialog chrome follows Windows language
- Final review (opus, 69c1919..b397526): Needs fixes — 1 Important (pinned toolbar after open from empty state); minors: UIA invoke during modal Open dialog, forwarded launch lost under Open dialog, README keys/same-window wording, kept-copy name Korean. Fix wave dispatched.
- Final fix wave: complete (commits b397526..793aacd: pinned toolbar refresh in commands::after, UIA/forwards held while Open dialog up, README fixes; re-review clean). Remaining minor: forwarded path dropped if user also picks a file in the Open dialog (pick wins, by design).

## Task 7 rulings
- R-T7a: Carry contradiction resolved in favour of Microsoft's documented pattern: on DBT_DEVICEQUERYREMOVE close the handle but keep the HDEVNOTIFY until REMOVECOMPLETE/QUERYREMOVEFAILED (matched by dbch_hdevnotify); all other paths unregister before closing. Review against this.
- R-T7b: 'until the user navigates' includes explicitly opening a file (Open dialog, drop, second instance forward) in the same folder after the drive returns — that must start the watch again. If the implementation only re-watches on a different folder, that is Important.

## Carry to M5
- Split oversized files: settings_win.rs (~973), uia.rs (~690 → uia_glue.rs), watch.rs (~738 → device_watch.rs), exif_card.rs (~790), input/commands/window.rs.
- with_com STA init once at startup; NOTICES copied beside exe in packaging; Open-dialog filter names refresh on language change.
- Settings cross-process lock / unique .tmp name; UIA robustness minors (Task 6 list); watch_loop panic handle leak.

# M5 status of the items carried to M5

Checked against the M5 branch (`m5-harden-ship`) in Task 9. The history above is kept as it was.

| M4 item | Status | Note |
|---|---|---|
| Split oversized files (settings_win, uia → uia_glue, watch → device_watch, exif_card, input, commands, window) | **M5에서 해결** (da802e6, 54b41ca, 197f512, 772b910, 883723e, 95272fc, 7ee8075, 2db101c) | Split by responsibility with no behaviour change; tests unchanged. |
| `with_com` STA init once at start-up | **M5에서 해결** (3ba0cff) | COM is initialized on the UI thread once; the per-call init/teardown is gone. |
| NOTICES copied beside the exe in packaging | **M5에서 해결** (bfcf3da) | Both the portable zip and the installer input carry `THIRD_PARTY_NOTICES.md` next to `light-view.exe`. Clicking About → [오픈소스 고지] in the packaged build was not driven (it would start the user's default .md program; README user-check). |
| Open-dialog filter names after a language change | **M5에서 해결** (3366bff) | Solved by holding the settings window while the Open dialog is up, so the language cannot change under a live dialog (a live IFileDialog cannot rename its filters). |
| Settings: unique .tmp name / cross-process lock | 일부 해결 (43e4d26) | Saves go through `settings.toml.<pid>.tmp`, so two processes never rename each other's half-written file. No cross-process lock (field-level saves from M4 remain). A stale `.<pid>.tmp` after a crash is not swept yet (Task 8 review minor). |
| UIA robustness minors (Task 6 list) | 일부 해결 (5008c6c) | `WM_GETOBJECT` after `WM_DESTROY` no longer recreates a provider, and the toast's [실행 취소] is exposed disabled while a dialog is open. The rest of the Task 6 list is still open. |
| watch_loop panic handle leak | 미해결 | Not changed in M5. |
