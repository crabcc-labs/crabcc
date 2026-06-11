// popup.js
const logsEl = document.getElementById('logs');
const relayInput = document.getElementById('relay-url');
const statusEl = document.getElementById('status');

chrome.storage.local.get('relayUrl', ({ relayUrl }) => {
  if (relayUrl) relayInput.value = relayUrl;
});
relayInput.addEventListener('change', () => {
  chrome.runtime.sendMessage({ type: 'setRelayUrl', url: relayInput.value });
  statusEl.textContent = 'relay URL saved';
});

async function renderLogs() {
  const { logs = [] } = await chrome.storage.session.get('logs');
  logsEl.innerHTML = '';
  logs.slice(-100).reverse().forEach(entry => {
    const div = document.createElement('div');
    div.className = entry.level;
    const preview = entry.args.slice(0, 3).map(a =>
      typeof a === 'string' ? a : JSON.stringify(a)
    ).join(' ').slice(0, 120);
    div.textContent = `[${entry.level}] ${preview}`;
    div.title = new URL(entry.url).hostname;
    logsEl.appendChild(div);
  });
  statusEl.textContent = `${logs.length} events captured`;
}

renderLogs();
setInterval(renderLogs, 1000);
