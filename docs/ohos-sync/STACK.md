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
32. `fonts: Report the load state of @font-face rules in FontFace.status` — B · CI · watch: tests/wpt/tests/mathml/support/fonts.js
33. `fonts: Only load @font-face fonts that the page uses` — C · CI · watch: components/shared/fonts/font_template.rs components/script/dom/document/document.rs tests/wpt/tests/mathml/support/feature-detection.js
34. `fonts: Only relayout for web font changes that can affect laid-out text` — B · CI · watch: -
35. `script: Lay out once for web fonts that finish loading together` — B · CI · watch: components/script/dom/globalscope/globalscope.rs components/net/image_cache.rs
36. `script: Delay the load event while web fonts are loading` — B · CI · watch: -
37. `storage: add an OHOS RDB backend for webstorage, client_storage and indexeddb` — C · CI+device · watch: components/storage components/config/prefs.rs components/net/Cargo.toml
38. `ci: Choose the pull request jobs from the changed files` — B · CI · watch: python/servo/try_parser.py .github/workflows/linux-wpt.yml
39. `ci: Smoke test the x86_64 HAP on the OpenHarmony emulator` — B · CI · watch: .github/workflows/ohos.yml support/openharmony/entry/src/main/module.json5
40. `constellation: Abort superseded iframe navigations` — B · CI · watch: -
41. `script: Add a js-quickjs feature for running on the QuickJS mozjs shim` — B · CI · watch: Cargo.toml components/script_bindings/trace.rs components/script/runtime/job_queue.rs components/script/scripting/script_module.rs
42. `script: Keep MediaQueryLists with change listeners alive` — B · CI · watch: -
43. `script: Treat any defined detail as present in performance.measure` — B · CI · watch: -
44. `ci: Build servoshell on QuickJS, and run WPT with it nightly` — A · CI · watch: .github/workflows/linux-wpt.yml .github/workflows/linux-common.yml .github/workflows/dispatch-workflow.yml python/servo/try_parser.py
45. `ci: Run the Linux build, lint and QuickJS jobs on the self-hosted runners` — B · CI · watch: .github/workflows/linux.yml .github/workflows/linux-arm64.yml
46. `mach: Add a --js-engine option to build on QuickJS` — B · CI · watch: python/servo/build_commands.py python/servo/testing_commands.py
47. `ci: Pass the check-run data to curl through stdin` — B · CI · watch: -
48. `wpt: Condition expectations on the JavaScript engine` — B · CI · watch: tests/wpt/tests/tools/wptrunner/wptrunner/metadata.py tests/wpt/tests/tools/wptrunner/wptrunner/manifestupdate.py tests/wpt/tests/tools/wptrunner/wptrunner/manifestexpected.py tests/wpt/tests/tools/wptrunner/wptrunner/wpttest.py tests/wpt/tests/tools/wptrunner/wptrunner/products.py tests/wpt/tests/tools/wptrunner/wptrunner/browsers/servo.py
49. `wpt: Add the QuickJS expectations` — B · CI · watch: -
50. `ci: Run QuickJS WPT on pull requests and fail on unexpected results` — B · CI · watch: .github/workflows/linux.yml .github/workflows/main.yml
51. `quickjs: Bump servo-quickjs for Intl over the system ICU` — B · CI · watch: -
52. `wpt: Accept repository names with hyphens in update-wpt run URLs` — B · CI · watch: -
53. `ci: Keep the QuickJS WPT run when a pull request needs a full run` — A · CI · watch: -
54. `quickjs: Bump servo-quickjs for Intl web compatibility` — B · CI · watch: -
55. `script: Release each microtask's autorelease pool entries when it ends` — B · CI · watch: -
56. `quickjs: Bump servo-quickjs for shared runtime-state access` — B · CI · watch: -
57. `servoshell: Move the headless window and event loop out of the desktop module` — B · CI · watch: -
58. `servoshell: Run the servoshell executable headless on OpenHarmony` — B · manual · watch: ports/servoshell/desktop/cli.rs ports/servoshell/running_app_state.rs ports/servoshell/build.rs
59. `wptrunner: Add a servo_ohos product for Servo on OpenHarmony devices` — B · manual · watch: tests/wpt/tests/tools/wptrunner/wptrunner/browsers/base.py tests/wpt/tests/tools/wptrunner/wptrunner/executors/executorservo.py tests/wpt/tests/tools/wptrunner/wptrunner/testrunner.py
60. `mach: Run WPT on OpenHarmony devices with mach test-wpt --ohos` — B · manual · watch: python/servo/command_base.py
61. `servoshell: Print a backtrace when the headless executable crashes on OpenHarmony` — B · manual · watch: ports/servoshell/panic_hook.rs
62. `script: Move to the end of the media resource at the end of the stream` — B · CI · watch: -
63. `script: Also keep media elements alive while they load or seek` — B · CI · watch: components/script/dom/document/document.rs
64. `gstreamer: Don't rewind a player that is paused at the end of the stream` — B · CI · watch: -
65. `script: Honor preload=none, and resume the load when playback is requested` — B · CI · watch: -
66. `media (ohos): Prepare the player of an audio element` — B · manual · watch: components/script/dom/html/embedded_content/htmlmediaelement.rs
67. `media (ohos): Report AVPlayer errors to the media element` — B · manual · watch: components/media/player/lib.rs components/script/dom/html/embedded_content/htmlmediaelement.rs
68. `media (ohos): Release AVPlayer when its player is dropped` — B · manual · watch: components/media/player/lib.rs components/script/dom/html/embedded_content/htmlmediaelement.rs
69. `media (ohos): Slide the playback buffer with the reads of AVPlayer` — B · manual · watch: components/media/player/lib.rs components/script/dom/html/embedded_content/htmlmediaelement.rs
70. `net: Do not keep media bodies that the HTTP cache cannot serve` — B · CI · watch: components/shared/net/request.rs
71. `net: Keep incomplete bodies out of the HTTP cache` — B · CI · watch: -
72. `script: Resume a backed-off media fetch at the next byte instead of seeking` — B · manual · watch: components/media/backends/gstreamer/player.rs components/media/backends/gstreamer/source.rs
73. `script: Keep queueing a media fetch without range support when the player is full` — B · manual · watch: components/media/backends/gstreamer/player.rs
74. `script: Treat a media fetch that backed off as a suspended download` — B · manual · watch: -
75. `script: Keep the origin of a media resource when its fetch restarts` — B · CI · watch: components/script/dom/canvas/imagebitmap.rs components/script/dom/canvas/canvas_context.rs
76. `gstreamer: Keep the offset of the source when it declines a seek` — B · CI · watch: -
77. `script: Don't pause the player of a media element that is potentially playing` — B · CI · watch: -
78. `script: Raise the ready state again when a seek leaves the end of the media` — B · CI · watch: components/media/backends/gstreamer/player.rs components/media/backends/ohos/player.rs
79. `script: Fix the inverted error check for the waiting event of media` — B · manual · watch: -
80. `script: Set the duration of an infinite media stream when it ends` — B · manual · watch: -
81. `net: Allow a fetch to be suspended and resumed` — B · CI · watch: components/net/connector.rs
82. `media (ohos): Refuse data that the playback buffer cannot take` — B · manual · watch: components/script/dom/html/embedded_content/htmlmediaelement.rs
83. `script: Suspend a media fetch instead of cancelling it when the player is full` — B · manual · watch: components/media/backends/gstreamer/player.rs components/media/backends/ohos/player.rs components/shared/net/fetch/fetch_controller.rs
84. `script: Cancel a suspended media fetch that nothing might resume` — B · manual · watch: components/shared/net/fetch/fetch_controller.rs
85. `media: Add a cache that fetches and keeps media resources for players` — B · CI · watch: -
86. `script: Add a fetcher of media resources for the media cache` — B · CI · watch: components/shared/net/lib.rs components/shared/net/fetch/fetch_controller.rs components/shared/net/request.rs
87. `script: Read the media resource of a media element from the media cache` — B · CI · watch: components/media/backends/gstreamer/player.rs components/media/backends/ohos/player.rs components/media/player/lib.rs
88. `script: Check the CSP of a media request before the media cache serves it` — B · CI · watch: -
89. `script: Let the media reader of a paused element idle outside a document` — B · manual · watch: components/media/backends/gstreamer/player.rs
90. `script: Fire stalled when a media download receives no data` — B · manual · watch: -
91. `script: Abort the media downloads of a document that stops loading` — B · manual · watch: components/script/dom/window/window.rs
92. `script: Run the error steps of media data once per player` — B · CI · watch: components/media/backends/ohos/player.rs
93. `script: Keep the media reader state of a media element on the heap` — B · CI · watch: -
94. `media (ohos): Read the media resource from the media cache` — B · manual · watch: components/script/dom/html/embedded_content/htmlmediaelement.rs
95. `servoshell (ohos): Put the media cache's file in the application's temporary directory` — B · manual · watch: components/config/prefs.rs
96. `media: Report the memory of the media cache` — B · manual · watch: components/profile/system_reporter.rs components/shared/profile/mem.rs
97. `ci: Build the GenUI-min configuration and report its size` — B · CI · watch: Cargo.toml ports/servoshell/Cargo.toml python/servo/package_commands.py .github/workflows/ohos.yml
98. `net: Rebuild the HTTP cache on http-cache-rules and a streaming store` — C · CI · watch: components/shared/net/request.rs components/net/connector.rs tests/wpt/tests/fetch/http-cache
99. `net: Make the disk the primary HTTP cache` — B · CI · watch: components/net/async_runtime.rs
100. `servoshell (ohos): Write the HTTP cache index when backgrounded` — B · manual · watch: -
101. `ports/arkweb: Use a disk-backed HTTP cache and persist its index on pause` — A · device · watch: components/servo/network_manager.rs
102. `ci: Run WPT on every ohos-main push and store the results in servo-ohos-wpt` — B · CI · watch: .github/workflows/linux.yml
103. `quickjs: Bump servo-quickjs for JS_GetGlobalFromScript` — B · CI · watch: components/script/scripting/module_loading.rs
104. `net: Add an owned stream type for response bodies` — A · CI · watch: -
105. `net: Take a TLS close without close_notify as the end of a close-delimited body` — B · CI · watch: components/net/connector.rs
106. `net: Carry the response body in the response instead of a done channel` — C · CI · watch: components/shared/net/lib.rs components/net/http_cache/tee.rs components/net/decoder.rs
107. `net: Check subresource integrity before handing the response over` — B · CI · watch: components/net/fetch/fetch_params.rs
108. `script: Error the body of a fetch() response on any network error` — B · CI · watch: components/script/dom/stream/readablestream.rs
109. `script: Reconnect an EventSource whose stream ends with a network error` — B · CI · watch: -
110. `script: Fail a style sheet whose body ends with a network error` — B · CI · watch: components/script/dom/html/htmllinkelement.rs
111. `script: Drop the received bytes of an XMLHttpRequest that ends with an error` — B · CI · watch: -
112. `script: Keep the fetch timing of a document whose body ends with a network error` — B · CI · watch: components/script/dom/performance/performancenavigationtiming.rs
113. `net: End a body cut short by the connection with a network error` — B · CI · watch: components/net/decoder.rs components/net/image_cache.rs
114. `quickjs: Bump servo-quickjs for interrupts and terminations in built-ins` — B · CI · watch: components/script/dom/promise/promise.rs components/script/scripting/script_execution.rs
115. `wpt: Add a crashtest for terminating workers that create promises` — A · CI · watch: -
116. `ci: Run WPT on the DAYU200 board of a self-hosted runner` — A · manual · watch: .github/workflows/ohos.yml .github/actions/setup-python/action.yml
117. `quickjs: Bump servo-quickjs for three QuickJS-only memory leaks` — B · CI · watch: components/script_bindings/reflector.rs components/script/dom/node/customelementregistry.rs components/script/runtime/script_runtime.rs
118. `ci: Run the device WPT with QuickJS and RDB, on a subset for GenUI` — A · manual · watch: ports/servoshell/Cargo.toml
119. `quickjs: Bump servo-quickjs for mozjs 0.26.9` — B · CI · watch: components/script_bindings/observablearray.rs
120. `ci: Run the device WPT in one chunk per DAYU200 board` — A · manual · watch: -
121. `ci: Double the nightly device WPT subset for two boards` — A · manual · watch: tests/wpt/include.ini
122. `deps: Bump mozjs to 0.26.14` — B · CI · watch: -
123. `quickjs: Bump servo-quickjs for mozjs 0.26.14` — B · CI · watch: Cargo.toml
124. `docs: Base ohos-main on Servo releases` — B · tooling · watch: -
125. `backport: wpt: Wait for web fonts before measuring in Ahem-based testharness tests (#49502)` — B · CI · watch: -

## Sync log

One entry per sync, newest first. Records the upstream range and every
patch that was dropped, superseded, squashed or split, with the reason.

### Stack debt

Open restack candidates, with the sync that proposed them. Each sync's fold,
or a restack between syncs, executes or rejects every entry.

- The media series (65–96): it conflicts on every sync (2026-10-22,
  2026-10-29, 2026-11-09: old 67, 88, 89, 90), and its later patches delete
  much of what earlier ones add (87 removes 550 lines of 72, 74, 83 and 84;
  94 removes 655 lines of 69 and 82). Proposed on 2026-10-22; deferred by
  the human on 2026-11-10 (2026-11-09 restack): it means rewriting patches
  and re-checking about 20 of them on a device.

### 2026-11-09 (restack, base unchanged)

- base: unchanged, `base/2026-11-09` (4e136d8ef31, Servo v0.8.0);
  pre-restack tip `pre-restack/2026-11-09` = cb51f8a94bb (=
  `sync/2026-11-09`), new tip `restack/2026-11-09`. Update branches with
  `git rebase --onto restack/2026-11-09 pre-restack/2026-11-09 <branch>`.
- why: the Stack debt entry "follow-up patches that fold cleanly into the
  patch they fix", which the human scheduled for a restack after the
  2026-11-09 sync. Stack 132 → 125; the final tree is cb51f8a94bb's apart
  from this manifest, the patch files and runbook decision 4.
- also carried, fixes of the sync after its push: the backport's message
  (upstream's message and `Signed-off-by` first and unchanged, the fork's
  notes and sign-off after them, on the human's review), runbook decision
  4 saying so, and the device unit tests in the 2026-11-09 entry.
- candidates: for every commit, `git blame` of the lines it removes,
  against the stack; a commit that rewrites the lines of one earlier patch,
  and whose message says it fixes or completes that patch, is a candidate.
  Each fold was dry-run as a `fixup` moved behind its target.
- fold (old numbers; each folded commit's text and trailers are kept in the
  target's message under "Folded in"):
  - 48 ← 53's code (the fixes of an independent review of 48's per-engine
    update); 53's runbook lines went to 49, whose section they extend.
  - 50 ← 55 (the token that 50's QuickJS WPT report needs).
  - 61 ← 124 (the launcher's oom_score_adj, so the kernel kills Servo).
  - 69 ← 98 (one AVPlayer failure reported once, not twice).
  - 83 ← 85 (net tells the consumer when it ignores a suspension).
  - 120 ← 121 and 125 (the device WPT workflow's skip and store steps).
- rejected: 96 (media reader state on the heap) into 90: 91–95 change the
  same fields of `HTMLMediaElement`, so the move conflicts.
- old → new: 1–52→1–52, 54→53, 56–84→54–82, 86–97→83–94, 99–120→95–116,
  122–123→117–118, 126–132→119–125.
- old 97's Validation named "the next patch" for 98's fix; it now names the
  patch that fix moved into.
- Stack debt: the folds entry is done (above) and leaves the list.
  `HTMLVideoElement`'s size in one place is rejected (human, 2026-11-10):
  one commit owning the size would leave `size_htmlvideo` failing at every
  commit before it, and the size test is to pass at every commit; the six
  `size_of.rs` conflicts return with upstream's #48031 and stay mechanical.
  The media series is deferred (human, 2026-11-10) and stays in the list.
- validation: the fold is tree-identical to the start; `./mach check` and
  `./mach check --ohos` (and `-p arkweb` from 4 on) pass at each of the 105
  commits that change code, and a locked `cargo metadata` at each of the 20
  that change Cargo files;
  `./mach test-scripts` at new 48 passes; `check_stack.py` on the restacked
  stack.

### 2026-11-09

- base: `base/2026-10-29` (ed6a9248aae) → `base/2026-11-09` = Servo v0.8.0
  (4e136d8ef31). ohos-main follows Servo's releases from this sync on (new
  `docs: Base ohos-main on Servo releases`). The tag is on a release branch:
  its branch point a19af13eeb9 (`release: Bump version to 0.8.0`, #49467)
  plus f10f711bc9c (Stylo from crates.io, 0.23.0) and 4e136d8ef31 (servo-svg
  not published). The merge queue's Main run of a19af13eeb9 passed. Pre-sync
  tip `pre-sync/2026-11-09` = 1ea6baf2d37. The mozjs 0.26.14 bump and the
  servo-quickjs ae9eb96 pin were validated as the branch quickjs-0.26.14 on
  ohos-main but not merged; they come in with this sync (new 129, 130). The
  dropped 2026-11-08 attempt, onto upstream main 0bf4c02a050, gave way to
  the release; its conflict resolutions, drift verdicts and fixups were
  reused where they apply.
- upstream: 70 commits, all but the two release commits within the range of
  the dropped attempt. Stack 136 → 132: the two `docs: record patches …`
  commits folded, the three placeholders of the 2026-10-29 sync dropped, the
  three `fixup!` commits that followed it (133–135) folded into 32, 33 and
  36, 40 emptied (decision 3), four patches added (new 129–132). Numbers
  below are the old ones unless marked "new".
- rebase: `git -c rerere.enabled=false rebase --signoff --empty=ask --exec
  '<cargo metadata --locked>'`; the lock check after every commit stopped at
  4 and 88, whose `Cargo.lock` had merged cleanly with the 0.7.0 workspace
  version of a crate they add. Fixes went on top as `fixup!` commits, and the
  fold after the validation was tree-identical (below).
- conflicts:
  - 4, 88 (stops of the lock check): arkweb and servo-media-cache take the
    workspace version 0.8.0 (#49467) in `Cargo.lock`. 4's lock, merged from
    the old patch, and new 129's `cargo update -p mozjs` also moved eight
    Windows-only `windows-sys` edges from upstream's 0.52.0 to 0.61.2; the
    independent review found them, and upstream's resolution is restored.
  - 31 (Stylo): v0.8.0 takes Stylo from crates.io (0.23.0, Stylo tag v0.23.0
    = the branch point's git pin 890aa121c6 with the versions bumped). The
    patch table moved into `[patch.crates-io]`, `update.sh` vendors the
    release tag of the required version (or a git pin, as before), the patch
    applied unchanged, `third_party/stylo` was re-vendored and `Cargo.lock`
    regenerated from upstream's: only the 11 Stylo crates lose their
    registry source.
  - 37, 88: the storage and media cache crates join the 0.8.0 workspace
    version block (#49467); 37 keeps `default-features = false`, and its
    `sqlite-backend` feature sits next to upstream's `subtlecrypto` (#48860).
  - 41 (js-quickjs): the task-end sweep sits in upstream's renamed
    `handle_messages()` loop; the feature next to upstream's `subtlecrypto`.
  - 67: its MANIFEST entry next to upstream's new media-blob-url-revoked test
    (#49206).
  - 89, 90: the import next to upstream's blob URL claim (#49206); 90 keeps
    the deletion of `fetch_request()`, which upstream gave the claim, since
    89's fetcher takes the same claim.
  - 102: `components/net/http_cache.rs` stays deleted (upstream's cleanup
    #49434 has no counterpart in the new cache).
  - 116: the status check wrapped upstream's steps 15.1 to 15.4 of
    initialising the document object; dropped around them, so the fetch
    timing with its redirect count is recorded whenever the document gets
    this far.
  - new 129 (mozjs 0.26.14, from quickjs-0.26.14): bumped from upstream's
    0.26.13; the lock changes only mozjs and mozjs_sys, as upstream's own
    bump (bbad078a6c2, after the release) does, once the `windows-sys` edge
    that `cargo update -p mozjs` also moved was set back (above).
  - Same resolution as in the dropped attempt for 37, 41, 89, 90, 102 and
    116, file for file.
- drift (`drift.py base/2026-10-29 base/2026-11-09`: 356 hits listed, 216
  hits of lockfile, workspace-manifest or WPT-manifest commits counted;
  pre-sync's drift.py listed 362;
  every hit is in the range the dropped attempt's agent review classified,
  except the two release commits, which only touch `Cargo.toml`,
  `Cargo.lock` and `components/svg/Cargo.toml`):
  - #49426 (aborting older pending changes of a navigable) → 40 superseded
    (below).
  - #49427 (`LoadStatus::Stopped`) and #49314 (`ConsoleLogLevel::Table`) →
    4 and 7: arkweb's exhaustive matches; a stopped load finishes like a
    completed one and reaches full progress, `console.table()` logs at INFO.
  - #48860 (`Crypto` always built) → 101: the GenUI-min rationale named
    WebCrypto and the `crypto` global; only SubtleCrypto is a feature now.
  - #45687 (mozjs 0.26.13, off-thread compilation) → 41, 101, 129 (new
    126) and new 129–130: Servo 0.8.0 can compile classic scripts off the main thread,
    behind the `dom_script_asynch` pref, off by default; the shim pinned by
    new 130 implements the stencil functions (Q50, Q51), which this sync's WPT
    runs don't exercise.
  - #49425 (`Window::suspend` calls `JS_MaybeGC` instead of `JS_GC`) → 122
    and QuickJS: the shim's `JS_MaybeGC` is a no-op, so a suspended window
    no longer collects with QuickJS. SpiderMonkey's collected once in 60
    suspends in a measurement of 2026-11-02; 122's "live realms" numbers were
    measured with the old `JS_GC`. Left as is (human, 2026-11-09: keep it,
    measure later).
  - the rest: context, compile-level, or manifest-only.
  - not in v0.8.0, unlike the dropped attempt: #49400 (27 stays), #48031
    (no attribute storage fix, no `HTMLVideoElement` size conflicts), #49686
    (MathML `dataset`), #49441, #49553, #49514 (mozjs 0.26.14 stays a fork
    patch), `/close-watcher` and `/compat` (not enabled).
- superseded this sync (decision 3, empty placeholder, removed next sync):
  - 40 → eb511d5c499 (#49426): its code still applies to 0.8.0 (#49553 isn't
    in it), but upstream does the same. Its Validation on plain 0.8.0 with
    its test added: 10/10, 25/25, 80/80; its five directories (512 tests): 8
    unexpected results: four that its own Validation found unexpected or
    flaky without it, history/001's subtest, flaky on this host, and the
    TIMEOUTs of replace-before-load's form-requestsubmit,
    form-submit-button-click and form-submit, the same with its code applied
    to 0.8.0 (A/B, three repeats each way). Upstream also aborts a pending
    change that reloads an entry (a traversal to a discarded entry, a crash
    page's replacement), which 40 left alone; no run exercises that. The
    specification sides with neither (a navigation of a traversing navigable
    is ignored); the human accepted upstream's behaviour and dropping 40's
    test with it (2026-11-09).
- Stack debt: the section is new, with the three restack candidates of
  2026-10-22 (`HTMLVideoElement`'s size in one place, the clean folds, the
  media series); the human decided on a restack after this sync, so this
  fold executes none.
- new 132 `backport: wpt: Wait for web fonts before measuring in Ahem-based
  testharness tests (#49502)`: above; applies empty on the first release
  that contains it.
- new 131 `docs: Base ohos-main on Servo releases`: runbook decision 1 and
  step 0, CONTRIBUTING.md, and `check_integrity.py`, which accepts a base that
  is a servo/servo release tag.
- old → new: 1..122→1..122, 126..131→123..128; new 129–132.
- dropped: 123–125 (placeholders of the 2026-10-29 sync), 132 and 136 (docs;
  quickjs-0.26.14's docs commit went the same way), 133–135 (folded into 32,
  33, 36).
- fork delta (`fork_delta.py`): 1226 → 1228 files. Left it (5): 40's code,
  test and its expectation of iframe-loading-lazy-reload-location-reload
  (superseded); the expectations of
  autoplay-disabled-by-permissions-policy.https.sub.html and
  operator-dictionary-empty-and-three-chars.html, which upstream's WPT sync
  (#49485) now has word for word (65's hunk keeps only the removal of the
  QuickJS conditions). Entered it (7): the expectation of
  font-face-unicode-range-no-match-no-download, which 33 makes pass, and
  new 132's five tests and the expectation of
  block-in-inline-self-collapsing-margin-001 that it removes. Changed (12):
  the resolutions of 31 (`Cargo.toml`, `update.sh`), 90
  (`htmlmediaelement.rs`), 102 (`http_cache.rs`) and 116 (`servoparser`),
  new 129's mozjs line in `Cargo.toml` and new 130's shim pin
  (`support/quickjs/cargo-config.toml`), the arkweb drift fixes
  (`runtime.rs`), and the public-text (`io_thread.rs`), `deploy.py`,
  `build_test_app.py`, GenUI (`justfile`) and CONTRIBUTING.md changes.
- expectations (stack effects, from the WPT runs below):
  - 33: font-face-unicode-range-no-match-no-download (#49485, FAIL upstream)
    passes. first-baseline-block-in-inline-001's table-cell subtest (#49485)
    fails with 33 and passes without it (bisected, 3 of 3 runs each): it
    measures while parsing, before Ahem has loaded, which 33 only fetches on
    use (it passes when it measures after the load event). A bisect of
    upstream after 0.8.0, with 32 and 33's code, found the fix: #49502
    (2b94d76a697) makes the test and four others wait for their font. On the
    human's decision, new 132 backports it unchanged; the five tests run as
    expected with both engines, with no expectation.
  - not carried from the dropped attempt: the MathML results of #49686, the
    replay.html and mo-form-dynamic-002 expectations and the QuickJS dialog
    conditions, which this run doesn't show on 0.8.0.
- QuickJS: a full run of the rebased stack (release, servo-quickjs ae9eb96,
  default ports): 61,175 tests, 80 with unexpected results, 5 crashes:
  load-removes-queued-error-event in every run, with both engines and on
  plain base (GStreamer on this host, as on 2026-10-29); moveBefore-iframe-
  crash, location_replace_session_history, stylesheet-appropriate-time-to-
  obtain and select-appearance-active didn't crash in the reruns. The 80 ran three
  times with each engine and once on plain base: QuickJS only: 1
  (window-open-multi-global, which also times out on plain 0.8.0 with
  SpiderMonkey on this host; no expectation); both engines: 36 (140 on
  2026-10-29), 33 the same on plain base and 3 stack effects: the two
  above and video-loading-lazy-autoplay-when-visible (TIMEOUT only with
  CI's timeout multiplier, as 64's message says); flaky: 43.
  mo-single-char-and-children fails on plain base too.
- validation:
  - gates on the rebased stack (code tree 25c715bacf, again on 0da59478ff
    after the review's `Cargo.lock` fixups, and on 6a4ab9a472 with new 132):
    clippy, crown, both
    OpenHarmony flavors (signed HAPs), unit and doc pass; tidy fails only on
    RUSTSEC-2026-0459 (libsqlite3-sys 0.36.0, also on upstream main) once a
    ruff finding in the new `check_integrity.py` code was fixed. Unit's
    test_webview_clear_history timed out once (20 s) and passed on the
    rerun, as during the mozjs PR's gates.
  - CI configurations: `./mach check --ohos -- -p servo-storage
    --no-default-features --features ohos-rdb-backend` and the GenUI headless
    QuickJS build pass.
  - `--js-engine quickjs` right after the rebase: no unused mozjs patch.
  - DAYU200 arkweb suite after deploy.py (returned 67 s after its reboot):
    37 passed; a run at 403 s of uptime lost the board when its hdcd
    restarted, the rerun at 478 s passed.
  - manual patches: 34; 4 re-run on this stack (27 and 104 on a HarmonyOS
    phone, 99 on the DAYU200, 94 headless on Linux), 22 waived with a reason,
    8 without a drift hit (`manual.tsv`). 99's file played to its end with an
    H.264 video; the dropped attempt's VP8 file doesn't decode on that board.
  - a dry run of the fold before the CI gate: tree-identical (161 → 138
    commits before the drops), with 40 empty. The fold of the gated
    6229d7f7148: tree-identical but for one sentence of the runbook (the
    Stack debt rule, reworded after the gate).
  - `check_integrity.py` (the `integrity` job) on the folded stack: 132
    patches reproduce the tip, both vendored copies are up to date, "base
    ok: servo/servo release v0.8.0"; with the Stylo table under its git
    source again, the new `[[patch.unused]]` check fails, as it should.
  - independent review, first pass (rebased stack): no blocker. Fixed: the
    #45687 wording of new 130 and the sync log, 40's evidence, the messages of
    the three `fixup!` commits between syncs (carried into 32, 33, 36), this
    log's crash claim, the `windows-sys` edges, and a `[[patch.unused]]`
    check in `check_stack.py`.
  - independent review, second pass (fold, messages, this log): no
    blocker. Fixed: this log's drift and fork-delta numbers, the watch
    paths of 32 and 33, 40's message (its previous Validation kept, which
    the new text refers to), names in new 129–130 that the pushed tree
    can't resolve, the rewording of the phone's name, 65's note, and the
    tiers and watch paths of the new patches.
  - CI gate, first candidate (25d3dfc6080), Linux: one unexpected result with
    both engines, box-decoration-break-clone-block-in-inline-001's "Cloned
    decoration still wraps the inline box fragments that do have content"
    (top -21.65 instead of -20: a fallback font's metrics), one of the five
    tests that new 132's backport makes wait for Ahem. The candidate with
    132 had no CI run: the human waived it (2026-11-09, minimal change); its
    five tests ran as expected locally with both engines.
  - CI gate, first candidate, DAYU200 (nightly subset, QuickJS): green, so
    every test ran (36,879). Against the stored run of 2026-11-05 (ohos-main
    1ea6baf2d37), two new CRASHes: shadowBlur_gaussian_tolerance.1, which
    crashed in 4 of the 5 stored runs before, and
    pointerevent_boundary_events_cross_document_same_origin.tentative, a
    hang ("Browser not responding") that upstream's WPT sync (#49485)
    expects as CRASH. Six new TIMEOUTs, of which
    moveBefore-range-iframe-crash timed out in 3 of the 4 runs before; 14
    tests with fewer passing
    subtests, 10 of them changed by #49485, the others network timing
    (dangling-markup, stale-while-revalidate's stale-css and stale-script,
    send-redirect); 4 tests no longer crash or time out.
  - every commit of the folded stack: `./mach check` and `./mach check
    --ohos` (and `-p arkweb` from 4 on) pass at each of the 109 that change
    Rust, Python, WebIDL, C++ or Cargo files, and a locked `cargo metadata`
    at each of the 20 that change Cargo files; the other 23 change none.
  - device unit tests on the DAYU200 (`./mach test-unit --ohos`, final
    tip, on the human's request): servo-storage 53/53 (sqlite), 64/64 (twin)
    and 43/43 (RDB only), one documented skip each; servo-fonts 2/3, with
    30's `autohinted_advances_match_loaded_advances` passing and upstream's
    `ohos::font_list::test_get_system_font_families` failing on this board
    (10 families instead of 1), as before the sync; the stack doesn't touch
    the OHOS font list.
- not run: 94's NETWORK_IDLE/HAVE_METADATA branch (no metadata headless;
  the partial re-run accepted by the human, 2026-11-09), 37's RDB A/B, 39's
  emulator smoke test.
- lessons, in the runbook: step 1 runs a locked `cargo metadata` after every
  commit (`rebase --exec`), which found the two lock files that merged
  cleanly but were stale, and says that a vendored crates.io crate of
  another version leaves Cargo building the unpatched one (new docs patch);
  the hdcd note has this sync's restart time (patch 1); the Stack debt
  list, which a sync's fold or a restack between syncs works off, and a
  recipe that needs a harness names a path in the repository (patch 1); a
  backport keeps upstream's message and `Signed-off-by` first, before the
  fork's notes and sign-off (decision 4, on the human's review of new 132).

### 2026-10-29

- base: `base/2026-10-22` (8cd4f60da15) → `base/2026-10-29` (ed6a9248aae);
  pre-sync tip `pre-sync/2026-10-29` = 63f2da0c73d. The merge queue's Main
  run of ed6a9248aae passed.
- upstream: 139 commits; stack 139 → 129: the seven `docs: record patches …`
  commits folded, the four placeholders of the 2026-10-22 sync dropped, three
  patches emptied (decision 3), one patch added (new 129). Numbers below are
  the old ones unless marked "new".
- rebase: `git -c rerere.enabled=false rebase --signoff --empty=ask`; 1–30
  applied cleanly; every rebased commit already ended in the gmail sign-off.
- conflicts:
  - 31 (Stylo): upstream moved the pin 633f43a → da29a2a (#48967); the patch
    applied unchanged, `third_party/stylo` re-vendored, `update.sh --check`
    ok, `Cargo.lock` regenerated from upstream's (the eleven `source` lines of
    the vendored crates). A re-export only changed the abbreviation of the
    index line, so the patch file was kept.
  - 50 (QuickJS expectations): two files resolved to upstream's:
    sandbox-window-open-srcdoc now times out with SpiderMonkey too, and
    #48658 (ConvolverNode) deleted convolver-response-1-chan.html.ini. The
    QuickJS conditions come from this sync's QuickJS run (below).
  - 60 (headless event loop): `desktop/event_loop.rs` keeps upstream's new
    `RepaintEventLoopWaker` (#49306, `rfd` dialogs) next to the moved loop.
  - 66 (media keep-alive): upstream landed a keep-alive of its own: #48979
    keeps playing or autoplay-eligible elements in a document set
    (`active_media_elements`) and pauses and resets them at unload, #49367
    releases them after `ended`. 66 had its own set. It now extends
    upstream's `should_keep_alive` with its rules (kept while delaying the
    load event, while seeking, not without a resource, autoplay only below
    HAVE_ENOUGH_DATA) and keeps its setter hooks; its set, flag and the hooks
    upstream also has are gone, and so is its +8 bytes of `HTMLVideoElement`.
    Renamed to `script: Also keep media elements alive while they load or
    seek`; message rewritten; watch gains `document.rs`.
  - 68 (preload=none): without 66's flag; its `autoplay` attribute arm split
    out of upstream's `autoplay | loop` arm.
  - 75, 76, 98: `HTMLVideoElement` size only; 88: the pref next to
    `mathml_core_enabled` (#48631), origin-clean in `CorsStatus` form, size;
    78: origin-clean from upstream's `CorsStatus` (#48748); 89: the
    suspended-fetch list without 66's list; 90: prefs next to
    `mathml_core_enabled`. The sizes (1208, 1216, 1224, 1256, 1168, 1096 at
    new 67, 74, 75, 86, 90, 96) were measured at each commit with a
    compile-time `size_of` probe through `./mach check`, calibrated against
    upstream's 1176 on the base.
  - 92 (media element reads the media cache): its keep-alive while
    downloading moved into upstream's `should_keep_alive`; upstream's unload
    cleanup closes the media reader where it cancelled the fetch, as the rest
    of 92 does; the removed fetch listener drops upstream's `CorsStatus` edit.
  - 114 (body in the response): upstream moved the CORS preflight cache into
    the fetch context (#49055, #49252), so `main_fetch`, `scheme_fetch`,
    `http_fetch`, `http_redirect_fetch` and the page content protocol
    handler's inner main fetch lose the cache argument as well as
    `done_chan`; the preflight fetch keeps upstream's devtools reporting
    (#49151); `to_filtered` takes the headers out before its body-less copy
    as upstream now does (#49313).
- compile fixes (folded): 91's media fetcher calls `cors_status()`, which
  replaced `is_cors_cross_origin()` (#48748); 114's `fetch()` passed one
  `done_chan` too many; 106's test helper sets `network_partition_key`
  (#49252); 114's `scheme_fetch` formatting. Found by the tip check, the
  drift review and test-tidy; the per-commit checks below pass.
- QuickJS (new 129): #48506 (ObservableArray) pins mozjs =0.26.9 and calls
  `CreateForwardingProxyHandler`, new in mozjs_sys 153.4. servo-quickjs
  c38af5c was mozjs 0.26.7, so Cargo left the patch unused and built
  `--js-engine quickjs` against SpiderMonkey's mozjs, which failed on the
  QuickJS-only calls of 42. servo-quickjs 18891c1 (unpushed) takes the 153.4
  bindgen output, implements the forwarding handler and is 0.26.9; new 129
  pins the tip of that branch, 544db84 (below).
- old → new: 1..40→1..40, 42..83→41..82, 85..101→83..99, 104..121→100..117,
  123..124→118..119, 126→120, 128→121, 130..132→122..124, 134..135→125..126,
  137..138→127..128; new 129 is `quickjs: Bump servo-quickjs for mozjs 0.26.9`.
- dropped (placeholders of the 2026-10-22 sync): 41, 84, 102, 103.
- superseded this sync (decision 3, empty placeholders, removed next sync):
  - 131 → 7615ccf8186 (#49187) and 132 → e78fc913f26 (#49218): identical
    patch-ids; 132 applies empty, 131 conflicts only because upstream moved
    ctutils on to 0.4.5 and is empty with upstream's lock.
  - 134 → 988d63c306a (#49329, WebRender 0.70.1): wr_glyph_rasterizer
    0.70.1 has servo/webrender#4892, the commit 134 pinned (same cfgs in
    `font.rs`, only a comment differs), plus the color emoji fixes.
- watch: 66 (new 65) gains `components/script/dom/document/document.rs`,
  where upstream's keep-alive set lives.
- drift verdicts (`drift.py base/2026-10-22 base/2026-10-29`, 609 hits from
  64 upstream commits plus 5 keyword hits; an agent review classified every
  hit, the full table is in the hand-off notes):
  - 08f70f7f907 ObservableArray, mozjs =0.26.9 (#48506) → 42, 105: the
    QuickJS build was broken (above, new 129); 105's GenUI-min builds on
    QuickJS too. The other mozjs APIs upstream started to call in this range
    (`JS_SetElement`, `SetArrayLength`, `ToUint32`, `CheckedUnwrapStatic`,
    `GetNonCCWObjectGlobal`) exist in the shim.
  - a833693925e `FetchMetadata` cleanup (#48748) → 91 (compile fix above),
    78, 88, 92, 114 (resolved above); 65–104, 74, 85, 87, 106, 107, 112,
    116–119: same semantics under the new name, or context. The
    `large_enum_variant` expects it removed sit over 85/87's
    `Suspend`/`Resume` variants: clippy below.
  - 37a706f350d (#48979), 812d2478cca (#49367) keep-alive → 66, 68, 89, 92,
    98 (resolved above). Upstream's unload step pauses kept-alive elements,
    closes their media reader (through 92) and resets their player, at every
    unload, bfcache entry included: 69–71, 96, 99, 100 (manual, OpenHarmony
    media) get the DAYU200 media WPT set below, with a navigation away from a
    playing element; 99's request-count scenarios and 96's
    `window.stop()`/`document.open()` checks were not re-run (below). The
    bfcache restore of a playing element fails on plain upstream
    (`paused` after the restore): upstream's regression, not the stack's.
  - 600d5df7610 CORS preflight cache partitioned (#49252), e933f9e7bee
    (#49055) → 114 (resolved above), 106 (test fix above), 73, 74, 85, 87,
    91, 107, 113, 115, 121: the fork builds no `RequestClient` outside 106's
    test; its fetches don't involve preflight.
  - 7210ba1f2c1 `rfd` dialogs (#49306) → 60 (resolved above); 15, 37, 42,
    101, 105, 135: context or desktop-only.
  - 74d4390985e (#48967, Stylo pin) → 31 (re-vendored).
  - 23d8a9ca5d6 ConvolverNode (#48658), 958f6687762 WPT sync → 50 (resolved
    above); MANIFEST.json elsewhere.
  - 64545cb831e MathML (#48631) → 88, 90 (resolved above); prefs context
    elsewhere.
  - 21a4125997f Rust 1.99.1 (#49310), ddad21d4741 OpenHarmony
    `-D warnings` in CI (#49368) → many: clippy, crown and the OpenHarmony
    checked-release build with `-D warnings` below.
  - 988d63c306a WebRender 0.70.1 → 134 (superseded, above);
    7615ccf8186 → 131, e78fc913f26 → 132 (superseded, above).
  - dc11d2eea18 Android media removal (#49265) → 4, 15, 27, 61, 101, 108:
    arkweb's media session calls are unchanged; the removed servoshell stubs
    were not overridden by the stack.
  - 3ab5a733b88 multiline textareas (#49369) → 15: arkweb's
    `convert_ime_options` now gets MULTILINE for textareas, as intended.
  - 5423aed16bb `webview_id` on the `handle_reflow` span (#49259) → 28: a
    span field; the RanLayout marker is unchanged.
  - 41230fbd7e8 `InputEvent` bitflags (#49269) → 15, 27: refactor; the
    composition events behave as before.
  - 5a6a8e169ef and its revert 38628af29ea (Android font matching) → 30,
    31, 33, 35, 42, 105: no net change.
  - all other hits: context, compile-level (the per-commit checks below) or
    `MANIFEST.json` only.
- QuickJS (new 129, 49): a full QuickJS run of the rebased stack against
  servo-quickjs 18891c1 (release, `--processes 14`, default ports): 60,788
  tests, 216 with unexpected results, 44 of them crashes. 36 crashes were
  "`SetArrayLength` is not implemented", every assignment to
  `adoptedStyleSheets` (ObservableArray sets the length of its backing
  array), and the page then aborted at exit: a document's finalizer clears
  the owner slot of its ObservableArray proxy, which QuickJS can finalize
  first in the same collection, and the shim's `SetProxyReservedSlot` on a
  finalized proxy panicked. servo-quickjs f1c371c implements
  `SetArrayLength`; e335588 ignores such a write during a cycle collection,
  where nothing can read the slot again (reads, and writes outside a
  collection, still abort). Servo's finalizer has the same order dependency
  on SpiderMonkey, where nothing guarantees the owner goes first; to report
  upstream. The 216 were then run three times with each engine
  (default ports, the fixed shim): only `load-removes-queued-error-event`
  still crashes, with both engines (GStreamer on this host). 6 tests had
  results that recurred in every QuickJS run and never occurred with
  SpiderMonkey. Three origin-keyed-agent-clusters tests that the WPT sync
  added (#49251) time out, as the directory's other tests do without
  WebAssembly; only those got `if js_engine == "quickjs"` conditions, in
  49, and then ran as expected with both engines. The other three failed
  because `Array.isArray` was false for ObservableArrays, whose proxies
  were not arrays to QuickJS: adoptedstylesheets-observablearray,
  idlharness (2 subtests) and CSSStyleSheet-constructable-concat (timed
  out). servo-quickjs 544db84 (patch Q44) gives QuickJS an `is_array`
  exotic method, which the shim's forwarding and wrapper proxies answer
  from their target, as SpiderMonkey does; with it the three pass with
  QuickJS, and the other 75 tests that use `adoptedStyleSheets` in
  included directories run as expected. New 129 pins 544db84 (the four shim
  commits are merged). The other 210 were flaky, failed with SpiderMonkey
  too, or were crashes of the old shim. A second full run against the same tree
  as 544db84, before it was merged,
  has no shim abort; of its 4 unexpected crashes,
  `load-removes-queued-error-event` is the known one and the other three
  were WebDriver harness errors that passed 5 of 5 reruns.
- validation (final stack unless noted):
  - test-tidy (cargo-deny, WPT manifests and lint, formatting; it found a
    formatting leftover of the 114 resolution, fixed), `./mach
    test-scripts`, check_stack.py and check_integrity.py.
  - every commit of the folded stack: `./mach check` (host) at the 108 with
    code, `cargo metadata --locked --offline` at the 19 that touch a Cargo
    manifest or the lock file, `./mach check --ohos` and `./mach check
    --ohos -p arkweb` at the 108 with code (arkweb with its 67–69 known
    devtools-off warnings). The `HTMLVideoElement` sizes above were measured
    at the six commits that set them.
  - `./mach clippy --locked -- -- --deny warnings`; `RUSTFLAGS="-D
    warnings" ./mach build --use-crown --locked --profile checked-release`
    (crown 1.99.1 built from `support/crown`); the same with `--ohos` (the
    Rust build; packaging stops at HAP signing, 00303116 short passwords in
    this container's signing config); `./mach test-unit --profile
    checked-release`: 1256 tests pass, `--doc` 46 pass.
  - host unit: servo-net at 106 (440), 114 (490) and the tip (499);
    servo-storage 53/53, arkweb 6/6.
  - QuickJS: release build against the pinned shim's tree (as a path
    patch, the commits being unpushed); full WPT run above.
  - DAYU200 arkweb suite (release shim, this sync's test app unchanged
    since 2026-10-22): 36 passed, 1 failed:
    `test_vsync.py::test_animation_advances_frames`, which then passed 3 of
    3 alone. A first run lost the board on hdc after deploy.py's reboot (all
    HdcDisconnectedError); `hdc kill -r` and a rerun.
  - DAYU200 OHOS WPT runner, headless release, SpiderMonkey and QuickJS:
    the media series' tests, loading-the-media-resource, error-codes and
    playing-the-media-resource, the autoplay permissions policy tests,
    played-loop, ready-states/autoplay, allowed-to-play, playing_media_no_gc
    and a scratch page that navigates away from a playing video and back (75
    tests): all as expected on both engines but the two autoplay permissions
    policy tests that pass where a timeout is expected (both engines, as on
    Linux), and the scratch page's bfcache-restore assertion (upstream's
    #48979 pauses at unload; it fails on plain upstream too); no crash, no
    Servo faultlog.
  - 134's equivalence on a DAYU200: the OpenHarmony checked-release build
    with `-D warnings` above (WebRender 0.70.1) has no FT_ symbols in the
    dynamic symbol table of its headless executable or libservoshell.so;
    `./mach test-wpt --ohos --profile checked-release css/css-backgrounds`
    (875 tests, 113 of which crashed on the assertion before 134): no crash,
    no `succeeded(res)` assertion, background-clip-002.html passes, 18
    reftests fail (12 with blank screenshots, a known board flake).
  - host WPT, release, against plain `base/2026-10-29`: media-elements,
    the-video-element, the-audio-element, fetch, xhr, eventsource, cors,
    subresource-integrity, resource-timing, canvas image drawing and
    imagebitmap, the mediaelementaudiosource tests and _mozilla/memory: 2,247
    tests, 19 unexpected on the stack and 14 on the base. The 9 differing
    tests and a navigate-away page ran 3 more times on both: the same on
    both (stale-while-revalidate css and script fail 3 of 3 on both under
    rerun load; autoplay-disabled-by-permissions-policy gives the same
    results, with 66's expectation on the stack), intermittent on both
    (media_fragment_seek, src_object_blob), or as 68's message says
    (video-loading-lazy-autoplay-when-visible fails without CI's timeout
    multiplier); fetch-metadata element-iframe and the cant-play-if-hidden
    tests passed 3 of 3.
  - not run: 99's request-count scenarios (bytes and requests per page
    load, loop and seek, over a rate-limited server) and 96's
    `window.stop()`/`document.open()` checks; the harness of those runs is
    not in the workspace. The navigate-away page above covers the new
    unload path. Also not run: the device unit tests of servo-fonts and
    servo-storage, 37's RDB A/B, the HarmonyOS phone checks of 26–28 (no drift
    against them), 39's emulator smoke test, and GenUI-min (105).

### 2026-10-22

- base: `base/2026-10-13` (5979b83191a) → `base/2026-10-22` (8cd4f60da15;
  first 964f0ebbe4c, see "second rebase"); pre-sync tip
  `pre-sync/2026-10-22` = 1773050c71b.
- upstream: 208 commits; stack 126 → 111: the eleven `docs: record patches …`
  commits and the two `squash!` commits folded, the three placeholders of
  the 2026-10-13 sync dropped, four patches emptied (decision 3), one patch
  added (new 111). Numbers below are the old ones unless marked "new".
- rebase: `git -c rerere.enabled=false rebase --signoff --empty=ask`; 1–30
  applied cleanly. Every rebased commit already ended in the gmail
  sign-off, so `--signoff` added nothing. ohos-main gained 125 and 126
  during the sync; they were cherry-picked onto the rebased stack without
  conflicts.
- second rebase: after the hand-off the stack moved from 964f0ebbe4c onto
  8cd4f60da15 for upstream's tokio 1.54.1 (#49161), which clears the
  cargo-deny advisory against tokio 1.54.0 that failed test-tidy on
  964f0ebbe4c with and without the stack. The other three commits (#48951,
  #49135, #49130) move script code to `TracedDOMString`. No conflicts;
  every commit is patch-identical (range-diff), and the tree moved by
  exactly upstream's change. Drift (new numbers): lockfile only at 4, 8,
  15, 30, 31, 37, 90, 91, 99, 104, 106, 107 and 109 (the tokio entry);
  keyword hit 2190554aaa6 (IndexedDB, script side): no patch calls the
  functions it changes, compile-level. Re-run on the new tip: test-tidy
  (passes, cargo-deny included), `./mach test-scripts`, check_stack.py and
  check_integrity.py; `cargo metadata --locked --offline` at the 18
  commits that touch a Cargo manifest or the lock file; `./mach check`,
  `./mach check --ohos`, `./mach check --ohos -p arkweb` (the same
  warnings as on 964f0ebbe4c), `./mach check --js-engine quickjs`, clippy
  `--deny warnings`, crown and OpenHarmony `checked-release` builds with
  `-D warnings`, `./mach test-unit --profile checked-release` (1226 pass)
  and `--doc`; host WPT of IndexedDB, FileAPI, eventsource,
  resource-timing, largest-contentful-paint, container-timing, webstorage
  and dom/nodes: 1799 tests, 4 unexpected, none of which recurs in three
  reruns (the partitioned localStorage test flips on plain 964f0ebbe4c
  too). The other results below are from 964f0ebbe4c.
- conflicts:
  - 31 (Stylo): upstream moved the pin c81cd9c → 633f43a (#48865, #48883,
    #48915, #48958, #49022, #49039, #49137; eight Stylo commits); the patch applied
    unchanged, was re-exported and `third_party/stylo` re-vendored,
    `update.sh --check` ok, `Cargo.lock` regenerated from upstream's. The
    patch only skips `::before`/`::after`; the `::first-line` that #49039
    enables behind a pref is not affected.
  - 37 (RDB storage): #48934 renamed servoshell's `default_without_allocator`
    to `default_base`, which mach now selects with `--no-default-features`
    for dummy-media builds, and #49065 moved `jemalloc` into it. Now
    `default_base` = `base` + `sqlite-backend`, and `base` takes upstream's
    list with `jemalloc`, so the RDB-only command of the message no longer
    names the allocator (a no-op on OpenHarmony anyway).
  - 44 (js-quickjs), 49 (mach `--js-engine`): the `js-quickjs` features next
    to the new `default` lines; mach no longer appends `media-gstreamer`
    (#48934), and `js-quickjs` follows the dummy-media block.
  - 52 (QuickJS expectations), 72 (its `squash!`): four hunks resolved to
    upstream's: the top-level-document subtest of
    autoplay-default-permissions-policy and resource-selection-candidate-moved
    now fail with SpiderMonkey upstream too (#49009), and #48972 deleted
    multisampled-stencil-renderbuffer-initialization.html.ini. The QuickJS
    conditions are regenerated from this sync's QuickJS WPT run.
  - 69, 71, 81, 82, 95, 100, 106: `size_of` of `HTMLVideoElement`, each
    patch's own change applied to upstream's smaller sizes (#48082); imports
    next to `TracedScriptResult` (#48855, 69, 96, 100) and `AtomicRefCell`
    (#49087, 100); 81's empty-chunk check under the new
    `process_response_chunk` signature (#49032).
  - 79, 80 (HTTP cache, media bodies and incomplete bodies): 80 merged with
    #48927 (most recent matching response, replacement of matching complete
    responses): `refresh` keeps upstream's `can_be_used_to_answer` lookup,
    which already skips aborted resources, plus 80's aborted check under the
    body lock; the insert drops abandoned resources before upstream's
    replacement. `test-unit -p servo-net` at 80: 403 pass.
  - 92, 94 (fetch suspension): `Suspend`/`Resume` next to the fetch thread's
    new `Cancel { forget }` (#49049); the body loop takes its timing from
    `fetch_params.timing_info` (#49040, #49046). `test-unit -p servo-net` at
    92: 412 pass.
  - 95, 98 (media prefs): next to upstream's removal of the
    `layout_unimplemented`, variable-fonts and writing-mode prefs (#48915).
  - 100 (media element reads the media cache): the fetch listener it removes
    had upstream's new listener signatures (#49032) and `LocalName` initiator
    (#48953); 99's fetcher now uses both (below).
  - 120 (HTTP cache rebuild): `http_cache.rs` deleted as before, which drops
    upstream's edits to it (#48927, #48842); the old cache steps of
    HTTP-network-or-cache fetch and `block_for_cache_ready` go as before
    with upstream's edits to them (#48927, #48867); `obtain_response` keeps
    upstream's `FetchParams` signature (#49040) and returns the body below
    the decoder as before. `test-unit -p servo-net` at 120: 440 pass, as
    before the sync.
- compile fixes for upstream API changes in code a later patch rewrites
  (found by checking every commit, folded as `fixup!` commits): 83 and 95
  pass the listener's global to `submit_timing` (#49032), and 99's fetcher
  uses the `LocalName` initiator (#48953) and the global of the element's
  document, since #49032 removed `resource_timing_global`, which returned it.
- drift fix: 25's geolocation xfail dropped (see a80dac85a93 below).
- fold: `squash!` 72 → 52 (the QuickJS conditions of the media tests that
  keeping media elements alive fixed) and 118 → 116 (the Bencher threshold
  of the GenUI-min sizes); each squash commit's description follows the
  target's, its Validation steps are appended to the target's, and its
  AI-assisted line is kept. The eleven `docs: record patches …` commits folded
  into this manifest. Placeholders of the 2026-10-13 sync dropped: 41
  (#48752), 42 (#48739), 60 (#48810). No reordering. The Validation
  paragraphs of 98–106, wrapped and separated from the trailers, are joined
  into the trailer block so that they parse.
- not done: dropping 79 and 80, which 120's fork note asked for. 92 builds on
  both (it calls 79's `may_store_media_response` in its body loop and sets
  80's abort flag), so dropping them means reworking 92 and re-resolving
  120; left for a maintainer decision, and 120's fork note now says so.
- old → new: 1..40→1..40, 43..59→41..57, 61..71→58..68, 73..75→69..71,
  77→72, 79..90→73..84, 92..96→85..89, 98..109→90..101, 111..112→102..103,
  114→104, 116→105, 120..123→106..109, 125→110; new 111 is
  `quickjs: Bump servo-quickjs for JS_GetGlobalFromScript`.
- dropped (decision 3, empty placeholders, removed next sync):
  - 43 → 98817dbcff5 (#48747), the landed version of the pull request 43
    backported: the same added and removed lines (the patch-ids differ only
    by context, since fb2540e8161 removed a field next to the hunks); applies
    empty. Its leak check was not re-run on plain upstream.
  - 90 → 7b7e3a54f98 (#48941): identical patch-id to that commit's
    `ports/servoshell/egl/app.rs` hunk; applies empty.
  - 111 → 5abd89f8ca3 (#48910) and 112 → 493d293e194 (#48994): identical
    patch-ids; apply empty.
- watch: no watched path moved in this range.
- drift verdicts (`drift.py base/2026-10-13 base/2026-10-22`):
  - a80dac85a93 geolocation (#48406) → 4, 6, 9, 10, 12–14, 24, 25, 32–37,
    40, 45, 69, 92, 95, 96, 98, 100, 104, 109, 114, 120: the permission
    prompt still reaches the embedder as a `PermissionFeature::Geolocation`
    request, which 25 turns into ACE's onGeolocationShow; positions now come
    from a `GeolocationDelegate`, which arkweb does not set (no OHOS location
    provider; before #48406 no position was ever delivered either). The
    geolocation prompt of test_permissions.py, an xfail until now, passes on
    the DAYU200 (4 of 4 runs): 25 drops the xfail and says so in its
    docstring and message.
  - 00e7faa71a5 online/offline from the embedder (#47568) → 4, 12, 13, 32–36,
    40, 43, 44, 69, 96, 100, 104, 114, 121, 123: a new `NetworkManager` API;
    arkweb's `NWeb::PutNetworkAvailable` is still a stub that could feed it.
    No effect.
  - ddd55f9146d GStreamer by default (#48934), 40f6fdc5cb2 jemalloc and
    sanitizers (#49065) → 4, 12, 13, 21, 37, 44, 49, 51, 64, 66, 109, 114,
    116: resolved in 37, 44 and 49; `media-gstreamer` is a no-op on
    OpenHarmony (`dep:` features on dependencies OpenHarmony does not
    declare), and GenUI-min passes `--no-default-features` with its own list.
  - 8ac71572444 RELR for OpenHarmony in mach (#49099) → 64, 116: 116's
    `just genui-min` passes `-z pack-relative-relocs` itself, so the flag now
    appears twice; harmless, removable from the justfile.
  - 152c2e077c7 mach no longer sets `CC`/`CXX` (#49066), 00a4022039b,
    3215155e7cf → 49, 64, 66: the OpenHarmony builds below (C++ of arkweb,
    mozjs, freetype) and `./mach test-scripts` pass.
  - 51d111766b2 fetch listeners get the global (#49032), 9abaf6066dd
    `LocalName` initiator (#48953) → the media patches 68–114, 32–36, 40,
    43–45: compile fixes in 83, 95 and 99 (above); the other listeners of the
    stack compile unchanged. Media WPT below.
  - d7639c0163b video poster (#48931), 2fed59308a5 `current_src` in an
    `AtomicRefCell` (#49087), d4f43258dda cues for video (#48809),
    2dcc9086379 `ignore_malloc_size_of` removals (#48864) → 68–114: merged
    without conflicts outside the imports above; media WPT below.
  - db49f29188c fetch-thread callbacks cleared at document teardown (#49049)
    → 35, 45, 87, 92, 94–96, 99, 100, 121: a terminated `FetchController`
    now cancels and forgets; 92's suspend and resume work on the same
    request id and are not called after a cancel. Media WPT and the net
    tests below.
  - 44deea18cfa (#48927), 0da9d282d30 (#48842), 57a263c02cc (#48867),
    b03045f53e4, 18165255dc2, 908d125f87c, d976443f341, a3afe0b7806,
    1d9563cc897, 8b8a8fe0156, bb6ff83b6cb → 79, 80, 92, 94, 120, 121:
    resolved above. #48927's new subtest in fetch/http-cache/vary.any.js
    (the most recent matching response) runs against 120's cache below.
  - 0d03c7bb9e5 webstorage skips setting an identical value (#49038) → 37:
    in the backend-independent manager, so the RDB environment gets the same
    skip; servo-storage unit tests below.
  - 06a86171345, 5ba2d078387 prefs → 10, 26, 37, 92, 95, 98, 109, 116, 120,
    121: resolved in 95 and 98; no effect on the proxy, RDB, media cache or
    HTTP cache prefs.
  - 170743882c6, 9fc2d931384, f040186a7ee, 375025af9f5 text controls and
    selection → 15, 27, 28, 33–36, 45, 87, 100, 104: no change to how
    composition text is inserted; 27 (manual) is checked on a HarmonyOS
    phone below, 15 by the device suite.
  - fb06a100aec smaller DOM elements (#48082) → 31, 44, 69, 71, 81, 82, 95,
    99, 100, 106, 116: `size_of` resolved above; `test-unit` checks it below.
  - 73b17372d6e, 777877f5f62 OpenHarmony font list → 29, 30: font list
    caching, not glyph metrics or freetype-sys; servo-fonts tests below.
  - 841e3cadba1, 10d93a9c95b safe wrappers in script_bindings, fb11f484542
    module leak, d58f03b4689 → 44: the QuickJS build below links them
    against the pinned servo-quickjs.
  - 0e726a77613 WPT sync, a37da32ff39 → 52, 72: resolved above.
  - 98817dbcff5 → 43, 7b7e3a54f98 → 90, 5abd89f8ca3 → 111, 493d293e194 →
    112: superseded (above); 7b7e3a54f98 → 27, 122: next to their lines,
    no effect.
  - 92cfba19fcc, 22b97d28ef4, f77e08a91b3, 521f418bf70, b2c56fe10b6,
    06a86171345, a3ae671843c Stylo pins, 79ee827b6a5 Taffy, 134c4b97fb8,
    a3b44798c9c → 31 (re-vendored), 44, 116: `Cargo.toml`/`Cargo.lock`
    context; Stylo WPT below.
  - 0a8b1347122, ccd3c1fc482, 1984c92d457, 8fd4f4d575a, 430a9e1b663,
    23564ef62e2, 3a244b44e31, afcc7cb50b7, e1d972943c6, e240216c943 and the
    other TracedDOMString and rooting changes, 26e779f3c60, bb3591a4a5c,
    5a9ef14f076, 4ba7263c558, fb2540e8161, 270fe198f82, 8ed51ffa536,
    9bda58e7818, 2473af37d77, 324bd4e51f0, ff72ab5cd0e, 7c1c278bf60,
    ba062b6cca9, 3b0bc6a3333, cbe9bd220b9, 6114b4042f1, 791082f8587,
    54d8e3d2745, bf74dd66549, 964f0ebbe4c → 15, 25, 28, 31–36, 40, 43–46,
    61, 87, 99, 100, 104: compile-level (32 and 33 differ in context only);
    crown, clippy and WPT below.
  - 471de19e467 backdrop-filter, bbfa12d9e32, 07dfb0f9eb4, 002642f4989,
    fb8affae77b, 24c74eaaee6, 4e336315839, c6947f2558f, bcad2174e48 and the
    other `tests/wpt` hits on 26, 33, 40, 46, 65, 69–71, 84, 86, 87, 120,
    121: shared prefs, `MANIFEST.json` and meta churn; test-tidy's manifest
    check passes.
  - fbe96d4b860 perf-analysis try job (#48807), 662409ffa91 `test-unit
    --no-run` → 14, 38, 47–49, 51, 53, 66, 98, 125: `./mach test-scripts` passes.
  - keyword hits: bf5ce38b12e ohos-media-sys 0.1.2 (the OpenHarmony media
    backend of 73–77, 93, 107, 108; lock-only), 946ae254065
    `InputEventType::Nothing`, 4d8f745ac46 indexeddb borrows, 60fc810362f
    cookie allocation: compile-level; the OpenHarmony checks pass.
- QuickJS (new 111, 50): the first full QuickJS run of the rebased stack
  crashed in 747 of 58,632 tests: since fb11f484542 (#48952)
  HostLoadImportedModule takes the referrer's settings object from
  `JS_GetGlobalFromScript`, which servo-quickjs did not implement. New
  patch 111 pins servo-quickjs a5ebf1f, which implements it (QuickJS patch
  Q35, the realm of a script's code). A second full run with it (release,
  `--processes 14`): 4 crashes, 120 tests with unexpected results. That
  run used non-default wptserve ports, which renames the subtests that
  contain the server URL; so the 120 were run again on the default ports
  once and then three times with each engine. 2 tests had results that
  recurred in every QuickJS run and never occurred with SpiderMonkey, and
  only those were updated (as `if js_engine == "quickjs"` conditions, in
  50): the new `fetch-body-does-not-leak-document.https.html` leak subtest
  of `Request.clone()` fails, and `about-blank-popup.https.sub.html` times
  out instead of failing. The other 118 were flaky, failed with
  SpiderMonkey too, or were port artifacts. Runbook step 3.8 (in 50) now
  says to run on the default ports. Stale QuickJS conditions on subtests
  whose names contain the port could not show up in the non-default-port
  run.
- validation (final stack unless noted):
  - fmt, test-tidy (cargo-deny, WPT manifest; the rustls-webpki advisories
    of the previous base are gone with #49124), `./mach test-scripts`,
    check_stack.py and check_integrity.py.
  - every commit of the folded stack: `./mach check` (host), and
    `cargo metadata --locked --offline` at the 18 that touch a Cargo
    manifest or the lock file; `./mach check --ohos -p arkweb` at 4–25 and
    123, `./mach check --ohos` at 26–123. The fixups of 83, 95 and 99 come
    from this check: the tip compiled while those commits did not.
  - `./mach clippy --locked -- -- --deny warnings`, `RUSTFLAGS="-D warnings"
    ./mach build --use-crown --locked --profile checked-release` and the same
    with `--ohos` (the Rust build; packaging the HAP fails in this container
    with hvigor 00308018, unrelated to the stack); `./mach test-unit
    --profile checked-release`: 1226 tests pass, `--doc` passes.
  - host unit: servo-net at the resolved 80 (403), 92 (412) and 120 (440,
    as before the sync); servo-storage 53/53, arkweb 6/6.
  - DAYU200 arkweb suite: 36 passed and 1 xpassed with this sync's release
    shim and test app; the xpass is the geolocation prompt of 25 (4 of 4
    runs), whose xfail is dropped (above). The shim exports 42 dynamic
    symbols: the 41 of the previous sync and 123's
    `application_backgrounded`.
  - DAYU200, OHOS WPT runner (headless release executable, 64–67): the
    media series' 60 tests (loading-the-media-resource, error-codes, the
    autoplay permissions policy tests, played-loop,
    ended-current-playback-position, gc-unreferenced-media-element,
    autoplay, allowed-to-play) for 73–77, 93, 107 and 108: all as expected
    but autoplay-default-permissions-policy, OK against its TIMEOUT
    expectation, as in 108's validation.
  - HarmonyOS phone (production HAP with `tracing-hitrace`): committed IME
    text appears in a text input (27); `--chrome=none` hides toolbar and
    tab bar (26), with the tracing filter passed next to it; hitrace records
    74 RanLayout markers against 75 `restyle_and_build_trees` and 76
    `handle_reflow` (28). The phone's hitrace level threshold was Info,
    which drops Servo's Debug-level spans: set `hitrace --trace_level D`
    for the capture (and back to Info).
  - host WPT, release, against plain `base/2026-10-22`: the HTTP cache and
    fetch set (79, 80, 92, 120), the media set (68–108), the Stylo set
    (31), web fonts with flexbox, grid and cssom-view (32–36), navigation,
    history, iframes, timers, workers, user-timing, performance-timeline,
    form submission and scripting-1 (40, 44, 46), webstorage and IndexedDB
    (37's sqlite side), and the _mozilla memory tests: 11,628 tests, 60
    unexpected on the stack and 82 on the base. The 26 tests that differed
    ran 3 more times on both: all flaky or the same on both, except 005.html,
    which passes with 40 (as at the previous sync). Alone, also against a
    pre-sync build: form-submit-cross-frame.html passes on all three
    (it fails only under load), jsurl-form-submit.tentative.html flips
    between runs on the stack and the base, and
    video-loading-lazy-autoplay-when-visible.html fails on all three
    without CI's timeout multiplier, as 68's message says.
  - 37 on devices: device unit tests on the DAYU200, servo-storage 53/53
    (sqlite), 64/64 (twin) and 43/43 (RDB only), one documented skip each,
    all warning-free. RDB-vs-sqlite WPT A/B on a HarmonyOS phone (release
    HAP with `ohos-rdb-backend`; scratch branch with the app commit of the
    `wpt-ohos` runner branch and mach's `--no-headless` allowed): 78
    tests, 1577 subtests, no subtest differs. The one test-level
    difference, `storage_local_setitem_quotaexceedederr.window.html`
    TIMEOUT on sqlite, passes on both in isolation (sqlite 9.4–9.5 s
    against the 10 s timeout, RDB 2.2–5.9 s). Only `*.rdb` stores after
    the RDB leg and only `*.sqlite` after the sqlite leg, besides the
    runner's first-launch `reg.rdb`. The runner's HAP mode watched the app
    with `kill -0`, which the shell may not send to an app on this phone,
    so up to half of the tests never ran; the A/B ran with a `/proc/<pid>`
    check instead.
  - not run: the device unit tests of servo-fonts; 39's emulator smoke
    test and actionlint (CI); the leak check of 43 on plain upstream.

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
    synthesizes. Device suite (taps, scroll, select, IME) and the phone's IME
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
  - HarmonyOS phone (production HAP with `tracing-hitrace`): committed IME
    text appears in a text input (27); `--chrome=none` hides toolbar and
    tab bar (26); with the tracing filter next to it, hitrace records
    RanLayout markers (7 against 10 `restyle_and_build_trees`, 28).
  - OHOS WPT runner (80–84), headless release executable on the phone:
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
  2026-09-13 shim passed them. HarmonyOS phone, production
  HAP with `tracing-hitrace ohos-rdb-backend`: soft-keyboard text reaches a
  page input (empty without patch 53), `--chrome=none` drops toolbar and tab
  bar, localStorage persists across a force-stop, IndexedDB accepts the same
  index name in two stores with either backend, the `RanLayout` marker
  appears in hitrace. OpenHarmony-flavor HAP packaged against an API-23 SDK
  runs on the DAYU200. `test_clipboard.py` 2/2 on the DAYU200. Patch 30's
  RDB-vs-sqlite WPT A/B on a HarmonyOS phone (release HAP with
  `ohos-rdb-backend`, the `wpt-ohos` runner branch cherry-picked): 78 tests,
  1577 subtests, no subtest differs; the one test-level difference,
  `storage_local_setitem_quotaexceedederr.window.html` TIMEOUT on sqlite,
  passes on both in isolation (sqlite 8.3–9.0 s against the 10 s timeout,
  RDB 1.2–1.4 s); store proof holds on both legs. The sqlite leg leaves
  60–70 IndexedDB databases registered whose `deleteDatabase` from test
  cleanup never completes, the RDB leg 3; plain upstream on the host leaves
  66 after the same tests, so this is upstream sqlite behaviour, not a
  stack regression. Patch 42, HarmonyOS phone, production HAPs of the stack
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
