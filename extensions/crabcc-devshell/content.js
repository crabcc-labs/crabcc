// content.js — injected at document_start into every frame
// Intercepts console.{log,warn,error,debug,info} and forwards to background.
(function () {
  const METHODS = ['log', 'warn', 'error', 'debug', 'info'];
  const original = {};
  METHODS.forEach(method => {
    original[method] = console[method].bind(console);
    console[method] = function (...args) {
      original[method](...args);
      try {
        chrome.runtime.sendMessage({
          type: 'console',
          level: method,
          args: args.map(a => {
            try { return JSON.parse(JSON.stringify(a)); } catch { return String(a); }
          }),
          url: location.href,
          ts: Date.now(),
        });
      } catch (_) {}
    };
  });
})();
