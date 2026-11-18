// META: title=Ensure multiple fetch do not crash the browser.
// META: timeout=long

promise_test(async () => {
  const requests = [];
  for (let i = 0; i < 5000; i++) {
    requests.push(new Promise(resolve => {
      const xhr = new XMLHttpRequest();
      xhr.open('GET', '/common/blank.html', true);
      xhr.onloadend = resolve;
      xhr.send();
    }));
  }
  await Promise.all(requests);
});
