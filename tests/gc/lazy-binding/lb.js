// Shared helpers for the lazy-binding tests in this directory.
//
// Every page loads this file with `<script src="lb.js"></script>`. It must not
// touch `document` or any other DOM object: pages measure the first handoff of
// those objects, so loading the helpers has to leave the counters untouched.

var LB = (function () {
  var checks = 0;
  var failures = 0;

  // Assert that `actual === expected`. Only `materialize_count()` deltas should
  // be asserted with this; `materialize_calls()` changes with any DOM->JSValue
  // conversion and is only useful for `calls > 0` style diagnostics.
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

  return { expect: expect, expectTrue: expectTrue, finish: finish };
})();
