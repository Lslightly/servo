// Shared helpers for the lazy-binding memory tests in this directory.
//
// Every page loads this file with `<script src="mb.js"></script>`. It must not
// touch `document` or any other DOM object: the pages measure what the parser,
// `createElement` and `insertAdjacentHTML` allocate, so loading the helpers has
// to leave the engine untouched.
//
// The pages read the engine counters directly and assert on them here:
//
//   var before = MB.record();   // allocate every record before the baseline
//   MB.counts(before);          // fill it: { objects, objectBytes, stubs, stubBytes }
//
// All four counters come from one `runtime.memory_usage()` call, which returns
// `[objects, objectBytes, stubs, stubBytes]` from a single
// `JS_ComputeMemoryUsage` heap walk: one `sizeof(JSObject)` per live object
// (64 bytes on 64-bit) and one 32-byte `JSDomStub` per stub.
//
// Two rules keep the snapshots comparable:
//
//   1. Allocate the `record()` objects (and any array the page keeps nodes in)
//      *before* the baseline snapshot. A snapshot record is a JSObject of its
//      own, so a record allocated after the baseline shows up in later counts.
//   2. Take every `counts()` snapshot a page needs *before* calling an
//      assertion. `expect*` prints through `console.log`, and console logging
//      itself allocates objects that stay alive, so a snapshot taken after a
//      log is off by a small constant (about 20 objects in a page that logs
//      between two snapshots).

var MB = (function () {
  var checks = 0;
  var failures = 0;

  // A snapshot record: a plain object with one slot per counter. Create one per
  // measurement point, all of them before the baseline (see the note above).
  function record() {
    return { objects: 0, objectBytes: 0, stubs: 0, stubBytes: 0 };
  }

  // Fill `out` with the engine counters and return it.
  //
  // One `memory_usage()` call covers all four counters with a single heap walk;
  // calling `obj_count()`/`obj_size()`/`dom_stub_count()`/`dom_stub_size()`
  // separately would walk the whole GC object list four times. The returned
  // array is counted *before* it is allocated, and it is released as soon as
  // this function returns (QuickJS frees by reference count), so it does not
  // perturb the next snapshot.
  //
  // It fills a record the page already allocated rather than returning a new
  // object or array: a returned container would itself be a live JSObject, so
  // every retained snapshot would add 1 to the `obj_count` the next snapshot
  // sees, and the exact deltas the pages assert on would be off by one per
  // snapshot.
  function counts(out) {
    var usage = window.runtime.memory_usage();
    out.objects = usage[0];
    out.objectBytes = usage[1];
    out.stubs = usage[2];
    out.stubBytes = usage[3];
    return out;
  }

  // The difference `after[field] - before[field]`.
  function delta(after, before, field) {
    return after[field] - before[field];
  }

  // Assert that `actual === expected`.
  function expect(name, expected, actual) {
    checks++;
    var ok = expected === actual;
    if (!ok) {
      failures++;
    }
    console.log(
      (ok ? "[PASS] " : "[FAIL] ") +
        name +
        " (expected " +
        expected +
        ", got " +
        actual +
        ")"
    );
  }

  function expectTrue(name, value) {
    checks++;
    var ok = value === true;
    if (!ok) {
      failures++;
    }
    console.log((ok ? "[PASS] " : "[FAIL] ") + name + " (got " + value + ")");
  }

  // Assert `min <= actual <= max`, for counts whose exact value depends on
  // parser-internal fragment bookkeeping.
  function expectRange(name, min, max, actual) {
    checks++;
    var ok = actual >= min && actual <= max;
    if (!ok) {
      failures++;
    }
    console.log(
      (ok ? "[PASS] " : "[FAIL] ") +
        name +
        " (expected [" +
        min +
        ", " +
        max +
        "], got " +
        actual +
        ")"
    );
  }

  // Print the per-page summary the runner greps for, then exit. A page that
  // throws before reaching here produces no summary and is reported as failed.
  function finish(name) {
    console.log(
      (failures === 0 ? "[PASS] " : "[FAIL] ") +
        name +
        " (" +
        checks +
        " checks, " +
        failures +
        " failures)"
    );
    window.close();
  }

  window.addEventListener("error", function (event) {
    checks++;
    failures++;
    console.log("[FAIL] uncaught error: " + (event.message || "unknown"));
  });

  return {
    record: record,
    counts: counts,
    delta: delta,
    expect: expect,
    expectTrue: expectTrue,
    expectRange: expectRange,
    finish: finish
  };
})();
