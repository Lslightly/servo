// Creates promises in platform code until terminated, for at most 10 seconds: the start promise of
// a stream and the promise of a read waiting for a chunk. Awaiting between batches runs the queued
// reactions, so the streams can be collected.
postMessage("started");
(async () => {
  const end = performance.now() + 10000;
  while (performance.now() < end) {
    for (let i = 0; i < 100; i++)
      new ReadableStream().getReader().read();
    await null;
  }
})();
