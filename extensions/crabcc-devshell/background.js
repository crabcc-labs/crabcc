// background.js — service worker
const MAX_LOGS = 500;

// Ring buffer in session storage
async function appendLog(entry) {
  const { logs = [] } = await chrome.storage.session.get('logs');
  logs.push(entry);
  if (logs.length > MAX_LOGS) logs.splice(0, logs.length - MAX_LOGS);
  await chrome.storage.session.set({ logs });
}

// Native host bridge (local Nix devshell)
let nativePort = null;
function ensureNativePort() {
  if (nativePort) return nativePort;
  try {
    nativePort = chrome.runtime.connectNative('com.crabcc.devshell');
    nativePort.onDisconnect.addListener(() => { nativePort = null; });
  } catch (e) {
    nativePort = null;
  }
  return nativePort;
}

// WebSocket bridge (wormhole relay)
let ws = null;
let wsUrl = null;
async function ensureWs() {
  const { relayUrl } = await chrome.storage.local.get('relayUrl');
  if (!relayUrl || (ws && ws.readyState === WebSocket.OPEN && wsUrl === relayUrl)) return ws;
  wsUrl = relayUrl;
  ws = new WebSocket(relayUrl);
  ws.onclose = () => { ws = null; };
  return ws;
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg.type !== 'console') return;
  const entry = { ...msg, tabId: sender.tab?.id };
  appendLog(entry);

  // Forward to native host if available
  const port = ensureNativePort();
  if (port) {
    try { port.postMessage(entry); } catch (_) {}
  }

  // Forward to WebSocket relay
  ensureWs().then(socket => {
    if (socket && socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify(entry));
    }
  });
});

// Relay URL update from popup
chrome.runtime.onMessage.addListener((msg) => {
  if (msg.type === 'setRelayUrl') {
    chrome.storage.local.set({ relayUrl: msg.url });
  }
});
