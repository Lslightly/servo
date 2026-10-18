# ohos-main patch stack

Ordered list of the patches on `ohos-main`, first patch first. Kept in sync
with `git log base/<date>..ohos-main` by `check_stack.py` (subjects must
match exactly). Per entry:

- tier per the fork policy: A = additive, B = seam in shared code,
  C = invasive;
- coverage: what proves the patch after a rebase — `CI` (host build/tests),
  `device` (the arkweb on-device suite), `manual` (only its `Validation:`
  recipe), `tooling` (self-checking scripts), `none` (docs);
- watch: upstream paths the patch depends on but does not modify. `drift.py`
  reports upstream commits touching them, so semantic drift is reviewed
  even when the patch applies cleanly. Keep the list file-level and short.

The checked-in `patches/` directory is generated from the stack by
`gen_patches.py` and verified by `check_stack.py`; never edit it by hand.
Its diff between syncs is the GitHub-viewable form of the range-diff.

Vendored third-party code under `third_party/<name>` is left out of the patch
files, which would otherwise carry megabytes of generated sources. Each
`third_party/patches/<name>/update.sh` regenerates its directory from upstream
plus the patches next to it, so after applying the stack to a fresh base, run
those scripts (they need network access). `check_stack.py` runs each with
`--check` to verify the committed copy.

1. `docs: add the ohos-main sync runbook and stack manifest` — A · tooling · watch: -
2. `docs: Mark this repo as an experimental OHOS downstream fork` — B · none · watch: -
3. `Allow clearly marked AI assisted contributions` — B · none · watch: -
4. `ports/arkweb: Servo as an alternative ArkWeb webview backend` — B · device · watch: components/servo/servo.rs components/servo/webview.rs components/servo/webview_delegate.rs components/shared/embedder ports/servoshell/egl/ohos
5. `ports/arkweb: vendor the arkweb-test app + a HAP build script` — A · device · watch: -
6. `ports/arkweb: Wire touch, scroll and key input; fix open/close leaks` — A · device · watch: components/shared/embedder/input_events.rs components/servo/webview.rs
7. `ports/arkweb: Drive console, progress and crash handler callbacks` — A · device · watch: components/servo/webview_delegate.rs
8. `ports/arkweb: Back the cookie manager with Servo's site-data manager` — B · device · watch: components/net/cookie_storage.rs components/servo/site_data_manager.rs
9. `ports/arkweb: Return ExecuteJavaScript results to ACE` — A · device · watch: components/servo/webview.rs components/servo/javascript_evaluator.rs
10. `ports/arkweb: Optional network proxy for on-device testing` — A · device · watch: components/config/prefs.rs
11. `ports/arkweb: restrict libservo_arkweb.so exported symbols and load it with RTLD_LOCAL` — A · device · watch: -
12. `ports/arkweb: drive presentation from the OHOS display vsync` — A · device · watch: components/servo/servo.rs components/servo/webview.rs
13. `ports/arkweb: honor LibraryLoaded lazy flag, defer Servo::new` — A · device · watch: components/servo/servo.rs
14. `ports/arkweb: split pure conversion helpers into a host-tested module` — B · CI · watch: components/shared/embedder
15. `ports/arkweb: IME / soft-keyboard integration` — B · device · watch: components/shared/embedder/input_events.rs components/servo/webview_delegate.rs ports/servoshell/egl/ohos components/script/dom/html/form_controls/htmlinputelement.rs components/script/dom/html/form_controls/htmltextareaelement.rs components/script/dom/document/editing.rs
16. `ports/arkweb: add on-device regression tests (scroll, nav, render, multi-webview, vsync)` — B · device · watch: -
17. `ports/arkweb: JS dialogs (alert/confirm/prompt)` — A · device · watch: components/servo/webview_delegate.rs
18. `ports/arkweb: <select> dropdowns` — A · device · watch: components/servo/webview_delegate.rs
19. `ports/arkweb: <input type=file> pickers` — A · device · watch: components/servo/webview_delegate.rs
20. `ports/arkweb: resolve <input type=color> (no ACE hook)` — A · device · watch: components/servo/webview_delegate.rs
21. `ports/arkweb: wire the OHOS system clipboard via ohos-pasteboard` — B · device · watch: components/servo/clipboard_delegate.rs components/servo/Cargo.toml components/script/dom/clipboard
22. `ports/arkweb: sync URL bar on history navigation` — A · device · watch: components/servo/webview_delegate.rs
23. `ports/arkweb: add port README with build/deploy/test docs` — A · none · watch: -
24. `ports/arkweb: wire Tier-1 NWeb methods` — A · device · watch: components/servo/webview.rs components/servo/webview_delegate.rs
25. `ports/arkweb: wire HTTP-auth and permission request delegates` — A · device · watch: components/servo/webview_delegate.rs components/script/dom/geolocation components/script/dom/permission
26. `ohos: add a chrome-less mode to the servoshell app` — B · manual · watch: ports/servoshell/prefs.rs
27. `ohos: Insert committed IME text through a composition update` — B · manual · watch: components/script/dom/document/editing.rs components/shared/embedder/input_events.rs
28. `layout: emit a trace marker when a reflow actually laid out` — B · manual · watch: components/shared/layout/lib.rs
29. `fonts: Compute FreeType glyph advances without the autohinter` — B · CI+device · watch: -
30. `third_party: vendor freetype-sys with the openharmony-rs patches` — B · CI · watch: components/fonts/platform/freetype
31. `third_party: vendor Stylo with the eager pseudo-element cascade skip` — B · CI · watch: Cargo.toml deny.toml components/layout/query.rs components/script/layout_dom/servo_layout_element.rs
32. `fonts: Report the load state of @font-face rules in FontFace.status` — B · CI · watch: -
33. `fonts: Only load @font-face fonts that the page uses` — C · CI · watch: components/shared/fonts/font_template.rs components/script/dom/document/document.rs
34. `fonts: Only relayout for web font changes that can affect laid-out text` — B · CI · watch: -
35. `script: Lay out once for web fonts that finish loading together` — B · CI · watch: components/script/dom/globalscope/globalscope.rs components/net/image_cache.rs
36. `script: Delay the load event while web fonts are loading` — B · CI · watch: -
37. `storage: add an OHOS RDB backend for webstorage, client_storage and indexeddb` — C · CI+device · watch: components/storage components/config/prefs.rs components/net/Cargo.toml
38. `ci: Choose the pull request jobs from the changed files` — B · CI · watch: python/servo/try_parser.py .github/workflows/linux-wpt.yml
39. `ci: Smoke test the x86_64 HAP on the OpenHarmony emulator` — B · CI · watch: .github/workflows/ohos.yml support/openharmony/entry/src/main/module.json5
40. `constellation: Abort superseded iframe navigations` — B · CI · watch: components/script/dom/html/embedded_content/htmliframeelement.rs
41. `script: Fix TimerListener leak (#48752)` — B · manual · watch: -
42. `script: ClearKeptObjects in microtask checkpoint (#48739)` — B · manual · watch: -
43. `script: Drop tasks for closed pipelines` — B · manual · watch: components/script/messaging.rs
44. `script: Add a js-quickjs feature for running on the QuickJS mozjs shim` — B · CI · watch: Cargo.toml components/script_bindings/trace.rs components/script/runtime/job_queue.rs components/script/scripting/script_module.rs
45. `script: Keep MediaQueryLists with change listeners alive` — B · CI · watch: -
46. `script: Treat any defined detail as present in performance.measure` — B · CI · watch: -
47. `ci: Build servoshell on QuickJS, and run WPT with it nightly` — A · CI · watch: .github/workflows/linux-wpt.yml .github/workflows/linux-common.yml .github/workflows/dispatch-workflow.yml python/servo/try_parser.py
48. `ci: Run the Linux build, lint and QuickJS jobs on the self-hosted runners` — B · CI · watch: .github/workflows/linux.yml .github/workflows/linux-arm64.yml
49. `mach: Add a --js-engine option to build on QuickJS` — B · CI · watch: python/servo/build_commands.py python/servo/testing_commands.py
50. `ci: Pass the check-run data to curl through stdin` — B · CI · watch: -
51. `wpt: Condition expectations on the JavaScript engine` — B · CI · watch: tests/wpt/tests/tools/wptrunner/wptrunner/metadata.py tests/wpt/tests/tools/wptrunner/wptrunner/manifestupdate.py tests/wpt/tests/tools/wptrunner/wptrunner/manifestexpected.py tests/wpt/tests/tools/wptrunner/wptrunner/wpttest.py tests/wpt/tests/tools/wptrunner/wptrunner/products.py tests/wpt/tests/tools/wptrunner/wptrunner/browsers/servo.py
52. `wpt: Add the QuickJS expectations` — B · CI · watch: -
53. `ci: Run QuickJS WPT on pull requests and fail on unexpected results` — B · CI · watch: .github/workflows/linux.yml .github/workflows/main.yml
54. `quickjs: Bump servo-quickjs for Intl over the system ICU` — B · CI · watch: -
55. `wpt: Accept repository names with hyphens in update-wpt run URLs` — B · CI · watch: -
56. `wpt: Keep both engines' expectations in update-wpt` — B · CI · watch: -
57. `ci: Keep the QuickJS WPT run when a pull request needs a full run` — A · CI · watch: -
58. `ci: Let the QuickJS WPT report create its check run and comment` — A · CI · watch: -
59. `quickjs: Bump servo-quickjs for Intl web compatibility` — B · CI · watch: -
60. `wpt: Run the ecmascript tests` — B · CI · watch: -
61. `script: Release each microtask's autorelease pool entries when it ends` — B · CI · watch: -
62. `quickjs: Bump servo-quickjs for shared runtime-state access` — B · CI · watch: -
63. `servoshell: Move the headless window and event loop out of the desktop module` — B · CI · watch: -
64. `servoshell: Run the servoshell executable headless on OpenHarmony` — B · manual · watch: ports/servoshell/desktop/cli.rs ports/servoshell/running_app_state.rs ports/servoshell/build.rs
65. `wptrunner: Add a servo_ohos product for Servo on OpenHarmony devices` — B · manual · watch: tests/wpt/tests/tools/wptrunner/wptrunner/browsers/base.py tests/wpt/tests/tools/wptrunner/wptrunner/executors/executorservo.py tests/wpt/tests/tools/wptrunner/wptrunner/testrunner.py
66. `mach: Run WPT on OpenHarmony devices with mach test-wpt --ohos` — B · manual · watch: python/servo/command_base.py
67. `servoshell: Print a backtrace when the headless executable crashes on OpenHarmony` — B · manual · watch: ports/servoshell/panic_hook.rs
68. `script: Move to the end of the media resource at the end of the stream` — B · CI · watch: -
69. `script: Keep media elements alive while they can still play` — B · CI · watch: -
70. `gstreamer: Don't rewind a player that is paused at the end of the stream` — B · CI · watch: -
71. `script: Honor preload=none, and resume the load when playback is requested` — B · CI · watch: -
72. `squash! wpt: Add the QuickJS expectations` — B · CI · watch: -
73. `media (ohos): Prepare the player of an audio element` — B · manual · watch: components/script/dom/html/embedded_content/htmlmediaelement.rs
74. `media (ohos): Report AVPlayer errors to the media element` — B · manual · watch: components/media/player/lib.rs components/script/dom/html/embedded_content/htmlmediaelement.rs
75. `media (ohos): Release AVPlayer when its player is dropped` — B · manual · watch: components/media/player/lib.rs components/script/dom/html/embedded_content/htmlmediaelement.rs
76. `docs: record patches 68 to 76 in the stack manifest` — B · none · watch: -
77. `media (ohos): Slide the playback buffer with the reads of AVPlayer` — B · manual · watch: components/media/player/lib.rs components/script/dom/html/embedded_content/htmlmediaelement.rs
78. `docs: record patches 77 to 78 in the stack manifest` — B · none · watch: -
79. `net: Do not keep media bodies that the HTTP cache cannot serve` — B · CI · watch: components/shared/net/request.rs
80. `net: Keep incomplete bodies out of the HTTP cache` — B · CI · watch: -
81. `script: Resume a backed-off media fetch at the next byte instead of seeking` — B · manual · watch: components/media/backends/gstreamer/player.rs components/media/backends/gstreamer/source.rs
82. `script: Keep queueing a media fetch without range support when the player is full` — B · manual · watch: components/media/backends/gstreamer/player.rs
83. `script: Treat a media fetch that backed off as a suspended download` — B · manual · watch: -
84. `script: Keep the origin of a media resource when its fetch restarts` — B · CI · watch: components/script/dom/canvas/imagebitmap.rs components/script/dom/canvas/canvas_context.rs
85. `gstreamer: Keep the offset of the source when it declines a seek` — B · CI · watch: -
86. `script: Don't pause the player of a media element that is potentially playing` — B · CI · watch: -
87. `script: Raise the ready state again when a seek leaves the end of the media` — B · CI · watch: components/media/backends/gstreamer/player.rs components/media/backends/ohos/player.rs
88. `script: Fix the inverted error check for the waiting event of media` — B · manual · watch: -
89. `script: Set the duration of an infinite media stream when it ends` — B · manual · watch: -
90. `servoshell: Drop an unmet lint expectation of the embedded app` — B · manual · watch: -
91. `docs: record patches 79 to 91 in the stack manifest` — B · none · watch: -
92. `net: Allow a fetch to be suspended and resumed` — B · CI · watch: components/net/connector.rs
93. `media (ohos): Refuse data that the playback buffer cannot take` — B · manual · watch: components/script/dom/html/embedded_content/htmlmediaelement.rs
94. `net: Report whether a fetch was suspended` — B · CI · watch: -
95. `script: Suspend a media fetch instead of cancelling it when the player is full` — B · manual · watch: components/media/backends/gstreamer/player.rs components/media/backends/ohos/player.rs components/shared/net/fetch/fetch_controller.rs
96. `script: Cancel a suspended media fetch that nothing might resume` — B · manual · watch: components/shared/net/fetch/fetch_controller.rs
97. `docs: record patches 92 to 97 in the stack manifest` — B · none · watch: -
98. `media: Add a cache that fetches and keeps media resources for players` — B · CI · watch: -
99. `script: Add a fetcher of media resources for the media cache` — B · CI · watch: components/shared/net/lib.rs components/shared/net/fetch/fetch_controller.rs components/shared/net/request.rs
100. `script: Read the media resource of a media element from the media cache` — B · CI · watch: components/media/backends/gstreamer/player.rs components/media/backends/ohos/player.rs components/media/player/lib.rs
101. `script: Check the CSP of a media request before the media cache serves it` — B · CI · watch: -
102. `script: Let the media reader of a paused element idle outside a document` — B · manual · watch: components/media/backends/gstreamer/player.rs
103. `script: Fire stalled when a media download receives no data` — B · manual · watch: -
104. `script: Abort the media downloads of a document that stops loading` — B · manual · watch: components/script/dom/window/window.rs
105. `script: Run the error steps of media data once per player` — B · CI · watch: components/media/backends/ohos/player.rs
106. `script: Keep the media reader state of a media element on the heap` — B · CI · watch: -
107. `media (ohos): Read the media resource from the media cache` — B · manual · watch: components/script/dom/html/embedded_content/htmlmediaelement.rs
108. `media (ohos): Report a failure of AVPlayer to the media element once` — B · manual · watch: components/script/dom/html/embedded_content/htmlmediaelement.rs
109. `servoshell (ohos): Put the media cache's file in the application's temporary directory` — B · manual · watch: components/config/prefs.rs
110. `docs: record patches 98 to 110 in the stack manifest` — B · none · watch: -

## Sync log

One entry per sync, newest first. Records the upstream range and every
patch that was dropped, superseded, squashed or split, with the reason.

### 2026-10-13

- base: `base/2026-10-08` (ab7e8fb1358) → `base/2026-10-13` (5979b83191a);
  pre-sync tip `pre-sync/2026-10-13` = 870190b0397.
- upstream: 79 commits; stack 85 → 67: the fourteen `docs: record patches …`
  commits and the four `squash!` commits folded, three patches emptied
  (decision 3). Numbers below are the old ones unless marked "new".
- rebase: `git -c rerere.enabled=false rebase --signoff --empty=ask`; 1–30
  applied cleanly.
- conflicts:
  - 31 (Stylo): upstream moved the pin 7b2f078 → c81cd9c (#48784, #48806;
    four Stylo commits); the patch applied unchanged, was re-exported and
    `third_party/stylo` re-vendored, `update.sh --check` ok. `deny.toml`
    resolved to upstream's: #46281 allows unused allowed organizations, so
    the patch no longer empties `[sources.allow-org]` and its message drops
    that paragraph. `Cargo.lock` regenerated from upstream's.
  - 36 (load event): `layout_api` imports next to upstream's
    `ContainerTimingRecord`; `LoadBlocker::terminate` takes the JSContext
    first since #48795 (compile fix).
  - 37 (RDB storage): `default_tooling` (#47255, devtools feature guard)
    kept in servo's `default` and servoshell's `base` group, whose contract
    is "the default set minus the storage backend".
  - 47 (TimerListener backport), 48 (ClearKeptObjects backport) → placeholders,
    see dropped.
  - 51 (js-quickjs): `components/servo/Cargo.toml` next to `default_tooling`.
  - 54 (QuickJS CI): `try_parser.py` workflow enum and names next to
    upstream's perf-analysis try job (#47820).
  - 62 (QuickJS expectations): `media-loading-pseudo-classes-in-has.sub.html.ini`
    resolved to upstream's (#48802 dropped its subtests); the QuickJS
    conditions are regenerated from this sync's QuickJS WPT run.
  - 74 (ecmascript tests) → placeholder, see dropped.
- fold: `squash!` 38 → 4 (OpenHarmony-v7.0-Release headers and
  ohos-patches/0002), 39 → 8 (partitioned-cookie overloads), 40 → 11
  (ohos-patches/0001 rebase), 65 → 56 (fork check for pull requests from
  branches of this repository); each squash commit's description follows
  the target's, its Validation steps are appended to the target's, and its
  AI-assisted line is kept. The fourteen `docs: record patches …` commits
  folded into this manifest. No reordering. Placeholders from earlier
  syncs: none left (the 2026-10-09 restack dropped them).
- runbook: step 3.4 restarts the hdc server after deploy.py's reboot, and
  a device-test gotcha covers the `BuildNotFound` of `mach build -p arkweb`
  (both in 1); step 3.8 gains the rerun rule this sync used for the QuickJS
  expectations (in 62, which owns step 3.8).
- old → new: 1..37→1..37, 42..43→38..39, 45→40, 47..49→41..43,
  51..54→44..47, 56→48, 58→49, 60..63→50..53, 67→54, 69..74→55..60,
  76→61, 78→62, 80..84→63..67.
- dropped (decision 3, empty placeholders, removed next sync):
  - 47 → 5f65837a45d (#48752) and 48 → 10c8149c3c8 (#48739): identical
    patch-ids (`git log --cherry-mark`); 47 stopped only because #48795
    rewrote the same lines. Their leak checks were not re-run on plain
    upstream.
  - 74 → 28d2258a4e1 (#48810): identical patch-id, applied empty.
- watch: 51's `components/script/modules/script_module.rs` →
  `components/script/scripting/script_module.rs` (#48801 moved it).
- drift verdicts (`drift.py base/2026-10-08 base/2026-10-13`):
  - a0d7a24dc0c devtools feature guard (#47255) → 4, 12, 13, 21, 26,
    32–37, 45, 49, 51, 52: arkweb depends on `servo` without default features,
    so it is now built without devtools, which it never used (a size win;
    add the `devtools` feature if `setWebDebuggingAccess` is wired up). The
    devtools-off build has upstream's dead-code warnings (68 locations, none
    on a line of this stack). servoshell keeps devtools through
    `default_tooling` (37 above). No effect otherwise.
  - f2fe72df95d a11y focused node (#48708) → 4, 6, 9, 12, 24, 28, 33–36,
    52: adds a private accessibility-focus step to `WebView`; no API the
    stack uses changed. Device suite (focus, controller tests) below.
  - 3e72b8874ab no pointer events for touch compatibility mouse events
    (#48701) → 4, 6, 14, 15, 27: `MouseButtonEvent` gained an internal
    flag; arkweb sends touch events, whose compatibility events script
    synthesizes. Device suite (taps, scroll, select, IME) and the PLR IME
    check below.
  - 54add76e6ec, 72437e50887 radio/checkbox state, 8d46a644803 cmd-delete
    (macOS) → 15, 27, 33: no effect on text input or composition.
  - e1fe321d8af ContainerTiming, 4b388ed8eaa Stylo prefs → 10, 28, 32–37,
    51, 52: pref and paint-metric additions; no effect on the proxy pref,
    the RanLayout marker (`restyle_and_build_trees` call unchanged) or the
    RDB pref.
  - d9e89624158 `modules` → `scripting`, 803fd928319 `FetchController`,
    667d03341f6 JSContext-first in `event_loop`, b237fc44f97 image
    `loadTime`, a34c92555aa computed role, bcc2d31b215 Element/Window spec
    steps, 8e64783f230 permanent roots, 34af31d71a6 `Serializable` → 28,
    32–36, 45, 49, 51, 52: compile-level only (36 fixed above); crown and
    WPT below.
  - 150b19a4e2b, 84088c7a435 Stylo pin and `display: contents` → 31
    (re-vendored), 51.
  - 82eb041cbeb Crypto to script_webcrypto, 979278bf00c vello, cb49d78d051
    deny → 31, 51: Cargo/deny context only.
  - 5f65837a45d, 10c8149c3c8 → 47, 48 (superseded); 5f65837a45d → 35 and
    10c8149c3c8 → 51, 76: next to the fixes, no effect; 76's per-microtask
    pool release sits beside ClearKeptObjects in the same checkpoint.
  - 3a942129b19 constellation `debug_assert_eq` restored (#48771) → 45:
    the assertion precedes 45's hunk and compares viewport sizes; no effect.
  - 3dcc4e02bc1 perf-analysis try job (#47820) → 14, 42, 54, 56, 58, 61,
    63, 65, 83: resolved in 54; `./mach test-scripts` below.
  - 5dd40514d14 taiki-e in lint.yml (#48776) → 56, 65: only in steps the
    self-hosted runners skip; no effect.
  - 28d2258a4e1, ee8fc660a33 ecmascript/acid WPT → 74 (superseded).
  - a7c2999d04e WPT sync and the other `tests/wpt` hits on 33, 45, 53, 62,
    82: shared `MANIFEST.json`/meta churn; test-tidy's manifest check passes.
  - keyword hits: c6a82e9db3f ohos-abilitykit-sys 0.1.7, 1d62cfd3615
    ohos-ime-sys 0.2.5 (arkweb IME uses ohos-ime; device suite below),
    330d3f91a15 Android keyboard: no effect.
- QuickJS expectations (62): a full QuickJS run of the rebased stack
  (release, servo-quickjs cbf02ee, `--processes 16`) ran 58,340 tests, 76
  with unexpected results. Those were rerun 3 times with each engine; 7
  tests had results that recurred in every QuickJS run and never occurred
  with SpiderMonkey, and only those were updated (all as
  `if js_engine == "quickjs"` conditions): six media tests of the known
  media-element lifetime gap (autoplay, allowed-to-play,
  resource-selection-candidate-moved; failing on ohos-main since the
  servo-quickjs pool scopes) and `sandbox-window-open-srcdoc.html`. Both
  engines time out on the latter; #48802 dropped its subtest expectations
  because SpiderMonkey reports no subtests (it hits wptrunner's outer
  timeout), while QuickJS still reports them as TIMEOUT and NOTRUN, as
  before the sync.
  The other 69 were flaky or failed with SpiderMonkey too (host effects).
  Afterwards the 7 ran as expected with QuickJS.
- validation (final stack unless noted):
  - fmt, test-tidy (cargo-deny, WPT manifest), `./mach test-scripts`,
    check_stack.py and check_integrity.py; `cargo metadata --locked` at all
    nine commits that touch a Cargo manifest or the lock file.
  - `./mach check`, `./mach check --ohos`, `./mach check --ohos -p arkweb`,
    `./mach check --js-engine quickjs`, `RUSTFLAGS="-D warnings" ./mach check
    --use-crown` and `./mach clippy --locked -- -- --deny warnings`; the only
    new warnings are upstream's devtools-off dead code in the arkweb build
    and upstream's unfulfilled `expect(unused_variables)` in servoshell's
    `egl/app.rs` (c73bea970da, before the previous base). Compile checks
    at the resolved commits 31, 36, 37 (also `--ohos` with the RDB
    backend) and 51.
  - host unit: servo-storage 53/53 (incl. schema_parity), arkweb 6/6,
    servo-fonts 7/7. Device unit (DAYU200): servo-storage 53/53 (sqlite),
    64/64 (twin) and 43/43 (RDB only), one documented skip each, all
    warning-free; servo-fonts 2/3 (upstream's
    `ohos::font_list::test_get_system_font_families`, as before).
  - DAYU200 arkweb suite: 35 passed, 1 xfailed (known) with this sync's
    release shim and test app; the shim exports the same 41 dynamic symbols
    as at the previous sync.
  - PLR (production HarmonyOS HAP with `tracing-hitrace`): committed IME
    text appears in a text input (27); `--chrome=none` hides toolbar and
    tab bar (26); with the tracing filter next to it, hitrace records
    RanLayout markers (7 against 10 `restyle_and_build_trees`, 28).
  - OHOS WPT runner (80–84), headless release executable on the PLR:
    `/infrastructure/` and `/dom/nodes/` (610 tests) run. Unexpected
    besides the testdriver BiDi/Bluetooth set: TouchEvent subtests of the
    two `Document-createEvent` tests and `assumptions/ahem.html` pass (the
    phone exposes touch events and has Ahem), `testdriver/file_upload.sub.html`
    fails, `moveBefore-range-iframe-crash.html` times out (load-flaky, as in
    82's validation), and `allowed-to-play.html` is reported CRASH ("Browser
    not responding", 3 of 3), as with the runner's pre-sync build; `moveBefore-disconnected-id-map.html` crashes as expected (its
    backtrace symbolizes to the `TreeOrderedIndexMap::remove` panic).
  - host WPT, release, against plain `base/2026-10-13`: the Stylo set (31),
    the web-font set with flexbox, grid and cssom-view (32–36), timers,
    navigation, history, iframes, form submission, workers, user-timing
    (45, 49), webstorage and IndexedDB (37's sqlite side): 9,811 tests,
    13 unexpected; 3 repeats on the stack and the base show the same flaky
    or host results on both, except 005.html, which passes with 45.
    `form-submit-cross-frame.html` failed 2 of 3 under load on the stack
    and 10/10 passes alone on both.
  - 49: 100 replace navigations (`nav2.html?how=timeout`), RSS 236 and 238
    MiB against 938 and 940 MiB on plain `base/2026-10-13`.
  - not run: 37's RDB-vs-sqlite WPT A/B on a device (step 4; needs the
    HAP-mode runner of branch `wpt-ohos`, which the stack's headless runner
    does not replace) — no upstream change touched components/storage in
    this range; 43's emulator smoke test and actionlint on the workflows
    (CI).

### 2026-10-09 (restack, base unchanged)

- base: unchanged, `base/2026-10-08` (ab7e8fb1358); pre-restack tip
  `pre-restack/2026-10-09` = c1ea354ab8c (= `sync/2026-10-08`), new tip
  `restack/2026-10-09`. Update branches with
  `git rebase --onto restack/2026-10-09 pre-restack/2026-10-09 <branch>`.
- why: the fold step run between syncs at the maintainer's request: drop the
  placeholders, fold follow-ups into the commits they complete, and reorder
  to the target order (docs/policy/CI first, additive early, invasive
  last). Stack 53 → 37; the final tree is c1ea354ab8c's apart from this
  manifest, the patch files and one runbook bullet on restacks.
- dropped (decision 3 placeholders, one sync early; `sync/2026-10-08` keeps
  them): 4 (API-21 SDK → fef77e189b8), 35 (FontFaceSet.load backport →
  a8d5fdaa915), 40 (rustls → 866d0957e4f), 48 (responsive-iframe
  expectations → f9b43a6f0b9).
- fold:
  - 1 ← 32 (stack conventions), 41 (integrity CI job), 43 and 50 (vendored
    code tooling), and the docs/ohos-sync parts of 31 and 52: all sync
    infrastructure now lives in patch 1, and no later patch touches
    docs/ohos-sync.
  - 2 ← the CONTRIBUTING.md part of 31 (it sits in text 2 adds).
  - 4 (old 5, ArkWeb backend) ← 6 (on-device render fixes, deploy.py, OHOS
    patches): the backend did not render on a device without them; their
    root-cause notes moved into the message.
  - 11 (old 12, exported symbols) ← 13 (dlopen with RTLD_LOCAL), renamed
    to say both.
  - 15 (old 17, IME) ← 18 (UpdateTextFieldStatus).
  - 30 (old 44, vendor freetype-sys) ← the freetype-sys part of 52, so the
    crate is vendored in the third_party/patches layout from the start.
  - 33 (old 37, lazy @font-face loading) ← 45 (no fonts for unloaded
    faces) and 47 (first available font of elements without text), both
    fixes of its regressions.
  - 35 (old 39, batched web font loads) ← 49 (SVG text re-rendered when a
    batch is applied); the SVG text gap remains at 33–34.
- moved: the test app (old 21) to right after the backend, so later
  features and their tests find it in the tree; servoshell (old 33, 53),
  the RanLayout marker (34), FreeType advances (42) and the two vendoring
  patches (44, 51) before the web-font series (36–39); the load-event delay
  (46, upstream-bound on its own) right after the series; the RDB backend
  (30, tier C) last.
- old → new: 1→1, 2→2, 3→3, 5→4, 21→5, 7..11→6..10, 12→11, 14..16→12..14,
  17→15, 19→16, 20→17, 22..29→18..25, 33→26, 53→27, 34→28, 42→29, 44→30,
  51→31, 36→32, 37→33, 38→34, 39→35, 46→36, 30→37.
- validation: the rebase applied with no conflicts; the final tree equals
  c1ea354ab8c outside docs/ohos-sync; `cargo metadata --locked` passes at
  every commit; `./mach check --ohos -p arkweb` at 4, 11, 15 and 25,
  `./mach check --ohos` at 26–27 and `./mach check` at 26–36 pass with no
  new warnings; `./mach test-unit -p servo-fonts` 7/7 at 29 and 33;
  check_stack.py, check_integrity.py and test-tidy at the tip. WPT at the
  web-font fold targets: see the restack's hand-off.

### 2026-10-08

- base: `base/2026-08-29` (bc6153bb1bb) → `base/2026-10-08` (ab7e8fb1358);
  pre-sync tip `pre-sync/2026-10-08` = 57bef201228.
- upstream: 701 commits; stack 61 → 53: the seven `docs: record patches …`
  commits and the two `squash!` commits folded, one new patch (53).
- rebase: `git -c rerere.enabled=false rebase --signoff --empty=ask` (the
  rr-cache held 85 recorded resolutions of unknown origin).
- conflicts:
  - 2 (fork notice): `main.yml` push preset vs #48530 (intel macOS jobs
    removed); kept the fork's reduced presets.
  - 4 (API-21 SDK): upstream fef77e189b8 (#48505) moved both products to
    API 23 → placeholder, see dropped.
  - 30 (RDB storage): workspace versions 0.7.0 (#48535) with the fork's
    `default-features = false` re-added; `multiprocess` (new upstream default,
    22c646903a0) kept in servo's `default` and servoshell's `base` group,
    whose stated contract is "the default set minus the storage backend";
    arkweb stays without it (single-process). The fork's `[lints.rust]` table
    dropped: upstream added `[lints] workspace = true` (a40515c3b0f), cargo
    rejects both, and `build.rs` already emits the check-cfg. `Cargo.lock`
    regenerated from upstream's (`ohos-rdb-sys` kept at 0.1.0).
  - 33 (chrome-less): upstream #48684 replaced `Tabs` with a `List` tab bar,
    a `Stack` of XComponents and `@Entry({ useSharedStorage: true })`;
    re-applied as `if (this.showChrome)` around the toolbar and the tab list.
  - 34 (RanLayout marker): upstream's accessibility work reshaped the
    `restyle_and_build_trees` call; marker re-added unchanged.
  - 35 (FontFaceSet.load backport) → placeholder, see dropped.
  - 36, 37 (web fonts): upstream converted `Rc<Promise>` to
    `RootedPromise`/`TracedPromise` (#47935, #48333) and `icu_locid` to
    `icu_locale_core`; resolved to the upstream-bound `lazy-webfonts` code
    (460f417f242, 3a1db33e3cc), which the code files of 36–39 now match
    line for line; 37's MANIFEST.json entry re-inserted.
  - 42 (FreeType advances): upstream d15ed10a8a8 (#48455) loads glyphs
    unhinted on Android and OpenHarmony. The fast path assumed light hinting
    and would have rounded advances that upstream now leaves fractional, so it
    is gated on light-hinted loads (desktop); the unit test asserts that and
    embeds its fonts so it also runs on devices. On OpenHarmony the patch now
    only skips decoding colour bitmaps; its OHOS measurements predate #48455.
  - 46 (load event): imports only. 51 (Stylo): upstream moved the pin
    b3e6425 → 7b2f078 (+368 Stylo commits); the patch rebased with identical
    `+`/`-` lines (context only: `clone_display()` → `*get_display()`),
    re-exported, `third_party/stylo` re-vendored, `update.sh --check` ok.
- applied cleanly, did not compile or did not work (fixed in the owning
  patch):
  - 5 (arkweb): `WebView::set_throttled` removed (c974e3c7c79) → `OnPause` /
    `OnOccluded` hide the WebView, which also stops painting; `WebView::focus`
    removed (15aa067b33e) → `set_focused`, focusing one WebView unfocuses the
    others as servoshell does with windows; blur stays a logged no-op.
    Applied to every arkweb commit, so each prefix builds.
  - 17 (arkweb IME): d2abb3339dd (#48690, landed the day of the base) made
    a composition end stop inserting text; the device suite caught it (typing,
    backspace and blur failed). Commits are now sent as update + end. The same
    bug hit servoshell's OpenHarmony port → new patch 53 (meant for upstream).
  - 30: index names unique per object store (#47936) mirrored into the RDB
    schema copy; `GenericCallback::new` lost its `ProfilerChan` (#48037).
  - 49 (SVG text): `traverse_preorder_non_rooting` →
    `traverse_preorder_unrooted` (#48555).
- fold: `squash!` commits into 30 (clippy `duplicate_mod` fix) and 36 (emoji
  expectations restored), this sync's fixups into their patches, the
  `docs: record patches …` commits into this manifest. Patches 49 (old 56),
  45–47 and 52 kept separate. Every commit now passes
  `cargo metadata --locked` (5–29 had auto-merged lockfiles cargo rejected).
  Each commit this sync resolved, adapted or regenerated carries an
  `AI-assisted:` line saying what (decision 5).
- dropped (decision 3, empty placeholders, removed next sync):
  - 4 → fef77e189b8 (#48505): the OpenHarmony product packages against an
    API-23 SDK and runs on a DAYU200; compatibleSdkVersion 20 → 23 is within
    the fork's minimum-API policy.
  - 35 → a8d5fdaa915 (#47564): identical patch-id (`--cherry-mark`).
  - 40 → 866d0957e4f (#48079): applied empty, base pins rustls 0.23.45.
  - 48 → f9b43a6f0b9 (WPT sync #47639): the edited `allowed=` variants no
    longer exist; base carries the `allow=` expectations.
- new tests: `components/storage/tests/schema_parity.rs` (30; fails on the
  pre-sync RDB schema), `test_focus.py`, repaint/tap after foregrounding in
  `test_throttle.py`, `test_cookies.py` with ControllerPage cookie buttons
  (28; first automated coverage of the cookie manager, 9),
  `test_clipboard.py` (25; replaces its manual round-trip recipe: a token
  written with `writeText` is read back after a force-stop, so only the
  system pasteboard can hold it, and without READ_PASTEBOARD the read falls
  back to the empty in-process clipboard).
- expectations: 37 drops its six `generic-family-keywords-003` canvas FAILs
  (they pass on the new base, as on upstream) and the two
  `layer-font-face-override` FAILs (passed with the lazy web-font patches
  already before this sync; upstream still fails them).
- drift (`drift.py`; hits not listed are churn in the large shared files
  covered by the compile and test gates):
  - 15aa067b33e system focus, c974e3c7c79 hidden WebViews, 07777aaa24a
    WebView borrows → 5 (above). fd4d5d0e377 key events, d2abb3339dd
    composition, 53f2a80314b touch compat mouse events → 7, 17: arkweb suite
    (IME fixed, touch/scroll/select/dialog pass).
  - 47def473014 `WebView::clear_session_history` → opportunity for 28's
    `DeleteNavigateHistory`, no drift. 12beb73b689 embedder JS in CSP
    documents → 10 gains it, no change needed.
  - c0e5583323a, 7256c05b108, 7130363bbf4 cookie host/domain/date → 9
    (manual): `test_cookies.py` added and passing.
  - 3eca066448f, d0162990069 clipboard promises/events → 25:
    `test_clipboard.py` added and passing.
  - e6aacc15323, b8fd8dbf768, 1462d866b8c, 2a8c5fab976, 84b5b2563a7 storage →
    30: only #47936 needed mirroring (done, schema test added); cache storage
    is in-memory and backend independent.
  - d15ed10a8a8, b724f6f80e1, b9ed512231f, 694133bbcf5 FreeType/metrics →
    42 (above); 31e3c3501e1, 29b00233c2c FontData → 45, 47: font WPT clean.
  - 31f660d20c5 layout image loads block `load` → 46: its WPT list
    (css-flexbox, css-grid, cssom-view) plus css-images and css-text clean.
  - cc0c73e197c, Rc<Promise> conversions → 36, 37, 49 (above).
  - Stylo upgrades fdb829e98c2, baa77ece393, cb086ca56ec, 5190fc46c70,
    614cd411f84 → 51 re-vendored; pseudo-element changes upstream do not
    touch the skip (`style_resolver.rs` hunk unchanged).
  - 64c4855e7dd, df3525edc0d ArkUI → 33 (above). a4fe23d8085 CMake step,
    da4d69a8af3 cargo-ohos → packaging works.
- validation (all on the final stack unless noted): fmt, test-tidy (incl.
  cargo-deny, WPT manifest), `./mach check` and `./mach check --ohos`,
  arkweb and servo-storage in the plain, twin and RDB-only configurations
  (warning-free); host unit tests servo-storage 53/53, servo-fonts 7/7,
  arkweb 6/6; device unit tests on the DAYU200 servo-storage 53/53 (sqlite),
  64/64 (twin) and 43/43 (RDB only), one documented skip each, servo-fonts
  2/3 (upstream's `ohos::font_list::test_get_system_font_families` fails on
  this board, as before the sync). WPT, host release build, against plain
  `base/2026-10-08`: css-font-loading, css-fonts, html canvas text,
  css-cascade 1115/1115; css-flexbox, css-grid, cssom-view, css-images,
  css-text 6564 with one unexpected (`cssom-view/resizeTo-negative.html`
  TIMEOUT, same on the base); the Stylo set (css-pseudo, css-content, cssom,
  transitions, animations, nesting, lists, selectors, css-cascade,
  css-variables) 2571, only the two layer-override passes above; SVG
  reftests, responsive-iframe and webstorage as expected; IndexedDB has a
  varying set of 1–5 worker/sharedworker timeouts per full-directory run on
  both builds (19 on the stack, 13 on the base over five runs each, the same
  tests, each passing in isolation). DAYU200
  arkweb suite 33 passed / 1 xfailed (geolocation, known) on the final
  shim; before the IME fix typing, backspace and blur failed while the
  2026-09-13 shim passed them. PLR-AL00, production
  HAP with `tracing-hitrace ohos-rdb-backend`: soft-keyboard text reaches a
  page input (empty without patch 53), `--chrome=none` drops toolbar and tab
  bar, localStorage persists across a force-stop, IndexedDB accepts the same
  index name in two stores with either backend, the `RanLayout` marker
  appears in hitrace. OpenHarmony-flavor HAP packaged against an API-23 SDK
  runs on the DAYU200. `test_clipboard.py` 2/2 on the DAYU200. Patch 30's
  RDB-vs-sqlite WPT A/B on the PLR-AL00 (release HAP with
  `ohos-rdb-backend`, the `wpt-ohos` runner branch cherry-picked): 78 tests,
  1577 subtests, no subtest differs; the one test-level difference,
  `storage_local_setitem_quotaexceedederr.window.html` TIMEOUT on sqlite,
  passes on both in isolation (sqlite 8.3–9.0 s against the 10 s timeout,
  RDB 1.2–1.4 s); store proof holds on both legs. The sqlite leg leaves
  60–70 IndexedDB databases registered whose `deleteDatabase` from test
  cleanup never completes, the RDB leg 3; plain upstream on the host leaves
  66 after the same tests, so this is upstream sqlite behaviour, not a
  stack regression. Patch 42, PLR-AL00, production HAPs of the stack
  without and with it, `servoperf page --only load`, two rounds of 5 loads
  in alternating order, `Layout>::reflow` M instructions/load: chuxing
  122.8 -> 89.9 (-27%), shibashiji 101.8 -> 94.3 (-7%), biyadi and
  keai-pruned unchanged within noise; the difference is colour-emoji PNG
  decoding (`Load_SBit_Png` 40.6 -> 7.3 M/load on chuxing).
- not run: the GenUI FCP A/B for 46 after 31f660d20c5.
- open: keep 42 (its colour-bitmap skip is still worth up to 27% of layout
  on emoji-heavy pages, see validation); patch 53 and the arkweb IME fix
  should go upstream with the next IME work; `lazy-webfonts` should drop the
  `layer-font-face-override` FAILs too.

### 2026-08-29

- base: `base/2026-08-07` (79e2c7a8827) → `base/2026-08-29` (bc6153bb1bb)
- upstream: 297 commits; stack 29 → 30 (new first patch: this runbook + manifest)
- conflicts: `ports/arkweb: wire the OHOS system clipboard via ohos-pasteboard`
  (servo-crate half now upstream 5c2a3839525 / #47177, patch shrunk to the
  arkweb glue, `clipboard` feature, READ_PASTEBOARD test-app permission;
  not a full supersede since upstream declined clipboard read);
  `storage: add an OHOS RDB backend …` (Cargo.toml vs #47198/#43819,
  `ascii_serialization` → `Cow` from #47264 mirrored in the twins).
- fold: nothing to do (no fixups pending; the clipboard shrink's empty-
  placeholder rule did not trigger — the patch shrank, it did not empty)
- dropped: none
- known stale: the RDB commit's 1.91 MiB RDB-only size saving no longer
  holds (#43819 makes rusqlite unconditional in components/net); remeasure.
- hygiene items: pre-sync `Cargo.lock` had a dangling `cookie 0.18.1` in the
  arkweb entry (`--locked` broken mid-stack); duplicated proxy comment in
  `ports/arkweb/src/runtime.rs`; three squashed commits keep an old
  `Validation:`/`AI-assisted:` pair mid-body.
- drift (from `drift.py`; hits not listed had no verdict-worthy content):
  - `b780274a164` clang-19, `058c55ed4a1` cargo-ohos build env → patch 4
    (API-21) unaffected textually; the rewritten packaging path is proven
    by the servoshell OHOS release builds. `./mach package --ohos` signing
    fails in the container (hvigor 00303116) — container config, not drift.
  - `c1517a950cc` ohos keyboard support → servoshell + xcomponent-sys only,
    no code shared with the arkweb IME patches (17, 18); device IME tests
    pass.
  - `27775d1d904` queued session-history traversals → potential behaviour
    change for patches 26 (URL bar follows history) and 28 (back/forward
    Tier-1 methods); `test_urlbar.py`, `test_controller.py` pass.
  - `6ee336ce836` MouseButton(s), `1728d63f488` a11y bounds,
    `522fa371e41` webgl guard, `a40c8f29010` threadboost, `e2bcea2a0f6`
    DPR override → embedder-API surface of patches 5, 7, 14–16 changed
    shape; `./mach check --ohos -p arkweb` warning-free and the device suite
    (render, scroll, vsync) passes. No behaviour drift observed.
  - `7a27057feec` cookie/auth memory reporting → patch 9 (cookie manager)
    compiles against unchanged `cookie_storage.rs` API; its manual
    Validation was NOT run this sync (no automated coverage) — open.
  - `cb680edc199` removed prefs, `8067519db8c`/`76f48dc9e3a`/`fb04f8f1613`
    pref additions → patches 11 and 30 set prefs by field name; compile
    proves none of the removed prefs were used.
  - `723dd93c61e`/`22b7b75ba42`/`522fa371e41` feature-flag reshuffle in
    `components/servo/Cargo.toml` → resolved in patches 25 and 30.
  - `5c2a3839525` clipboard backend → patch 25 partially superseded (see
    conflicts). `7bea9ed91f2` `Cow` origins → mirrored in patch 30.
    `fb04f8f1613` disk cache → patch 30 size rationale stale (see above).
  - `b8819327242` quick_cache, `e0b630fe3e0` aws-lc-rs SRI, `27a317dd69c`
    disk-cache temporary storage → `components/net/Cargo.toml` churn with
    no storage interaction.
  - keyword scan: `cb9b0804bda`/`748bf81026f` keyCode changes and
    `f9b4d1e6094` insertLineBreak are script-side (page-visible, not port);
    `c65518f8960`/`2a5f73586c3` CookieStoreManager is the DOM layer, not
    the NWeb cookie manager; `b3ee31d291c` media ohos dummy backend,
    `eff9ff0bfa4` OHOS font fallback, `e8ffe8e88ce` image decoder trait,
    `ba14b47a9a6` arboard wayland → not on the stack's surface.
- validation: check --ohos, tidy, fmt, test-unit servo-storage 53/53,
  arkweb release build + deploy, DAYU200 arkweb suite 30 passed / 1 xfailed,
  device nextest servo-storage sqlite 53/53 and RDB 43/43 (+1 documented
  skip), clipboard round-trip with/without READ_PASTEBOARD, servoshell
  release with and without `ohos-rdb-backend`, RDB-vs-sqlite same-HAP pref
  A/B on localhost origin, and the full WPT subset A/B via
  `./mach test-wpt --ohos` (runner from `wip-wpt-ohos` + device fixes on
  scratch branch `ohos-sync-2026-08-29-wpt`): 78 tests, 0 differing
  subtests, store proof both legs. `--flavor=harmonyos` does not install on
  the DAYU200 (profile UDID); default flavor, hap-sign-tool signing.
- found during validation: servoshell race `webdriver.rs:171 Expected at
  least one window to be open` when WebDriver answers before
  `on_surface_created`; `hdc file send -b` fails on the DAYU200 (hdc
  3.2.0b) — worked around in the runner, servoshell fix pending.
