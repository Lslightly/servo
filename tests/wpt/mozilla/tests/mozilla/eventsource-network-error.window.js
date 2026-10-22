async_test(t => {
  const source = new EventSource("resources/eventsource-network-error.py");
  t.add_cleanup(() => source.close());
  const events = [];
  source.onopen = t.step_func(() => {
    events.push("open");
    if (events.length > 1) {
      assert_array_equals(events, ["open", "message", "error", "open"]);
      t.done();
    }
  });
  source.onmessage = t.step_func(() => events.push("message"));
  source.onerror = t.step_func(() => {
    events.push("error");
    assert_equals(source.readyState, EventSource.CONNECTING, "readyState");
  });
}, "EventSource reestablishes the connection when the event stream ends with a network error");
