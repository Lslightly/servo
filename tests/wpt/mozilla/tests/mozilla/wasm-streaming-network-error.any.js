// META: global=window,dedicatedworker

for (const [error, description] of [
  ["decode", "fails to decode"],
  ["cut-short", "is cut short"],
]) {
  promise_test(async t => {
    const response = fetch(`resources/wasm-network-error.py?error=${error}`);
    await promise_rejects_js(t, TypeError, WebAssembly.compileStreaming(response));
  }, `compileStreaming rejects with a TypeError when the body ${description}`);
}
