// Shared helpers for the lazy-binding memory tests in this directory.
//
// Every page loads this file with `<script src="mb.js"></script>`. It must not
// touch `document` or any other DOM object: the pages measure what the parser,
// `createElement` and `insertAdjacentHTML` allocate, so loading the helpers has
// to leave the engine untouched.

var MB = (function () {
  var checks = 0;
  var failures = 0;

  // Sample `window.runtime.memory_stats()` and print a marker naming the sample.
  //
  // `memory_stats()` appends one row to the memory-stats CSV and returns
  // undefined, so the marker is how the runner pairs a CSV row with the
  // checkpoint it was taken at. The row is written first and the marker printed
  // second; the two orders are the same, so the i-th `[STATS]` line in the log
  // names the i-th page row of the CSV.
  //
  // The engine also appends two rows when the runtime is destroyed (after
  // `finish()` closes the window); the runner ignores those.
  function sample(label) {
    window.runtime.memory_stats();
    console.log("[STATS] " + label);
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
    sample: sample,
    expect: expect,
    expectTrue: expectTrue,
    expectRange: expectRange,
    finish: finish,
  };
})();
