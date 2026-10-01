const HTTP_BASE = 'http://127.0.0.1:8765';
const SSE_URL = `${HTTP_BASE}/events`;
const WS_URL = 'ws://127.0.0.1:8765/socket';
const byId = id => document.getElementById(id);
let eventSource = null;
let socket = null;
let nodeBatch = 0;
let taskNumber = 0;
let popoverOpen = false;
let socketMessageNumber = 0;
let sseMessageCount = 0;
const sseLines = [];
const socketLines = [];

function writeLog(id, lines, entry) {
  lines.push(entry);
  if (lines.length > 12) lines.shift();
  byId(id).textContent = lines.join('\n');
}

byId('fetch-items').addEventListener('click', async () => {
  const query = byId('query').value;
  byId('fetch-state').textContent = `Loading: ${query}`;
  try {
    const response = await fetch(`${HTTP_BASE}/api/items?q=${encodeURIComponent(query)}`);
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const payload = await response.json();
    const list = byId('items');
    list.textContent = '';
    for (const item of payload.items) {
      const row = document.createElement('li');
      row.className = 'item';
      row.textContent = item.label;
      list.appendChild(row);
    }
    byId('fetch-state').textContent = `Loaded ${payload.items.length} items for ${payload.query}`;
  } catch (error) {
    byId('fetch-state').textContent = `Fetch failed: ${error.message}`;
  }
});

byId('churn-items').addEventListener('click', () => {
  const list = byId('churn-list');
  list.textContent = '';
  nodeBatch++;
  const count = nodeBatch % 3 === 0 ? 0 : 12;
  for (let index = 0; index < count; index++) {
    const node = document.createElement('span');
    node.className = 'churn-node';
    node.textContent = `batch ${nodeBatch} - node ${index + 1}`;
    list.appendChild(node);
  }
  byId('fetch-state').textContent = `DOM churn batch ${nodeBatch}: ${count} nodes`;
});

byId('sse-connect').addEventListener('click', () => {
  if (eventSource && eventSource.readyState !== EventSource.CLOSED) return;
  eventSource = new EventSource(SSE_URL);
  eventSource.addEventListener('open', () => { byId('sse-state').textContent = `SSE connected | events: ${sseMessageCount}`; });
  eventSource.addEventListener('progress', event => {
    const payload = JSON.parse(event.data);
    sseMessageCount++;
    writeLog('sse-log', sseLines, `${payload.sequence}: ${payload.message}`);
    byId('sse-state').textContent = `SSE connected | events: ${sseMessageCount} | last event ${payload.sequence}`;
  });
  eventSource.addEventListener('error', () => {
    byId('sse-state').textContent = eventSource.readyState === EventSource.CLOSED ? 'SSE closed' : 'SSE reconnecting';
  });
});

byId('sse-close').addEventListener('click', () => {
  if (eventSource) eventSource.close();
  eventSource = null;
  byId('sse-state').textContent = 'SSE disconnected';
});

byId('ws-connect').addEventListener('click', () => {
  if (socket && socket.readyState < WebSocket.CLOSING) return;
  socket = new WebSocket(WS_URL);
  socket.addEventListener('open', () => { byId('ws-state').textContent = 'WebSocket connected'; });
  socket.addEventListener('message', event => {
    writeLog('ws-log', socketLines, String(event.data));
    byId('ws-state').textContent = `WebSocket connected | reply: ${String(event.data)}`;
  });
  socket.addEventListener('error', () => { byId('ws-state').textContent = 'WebSocket connection error'; });
  socket.addEventListener('close', event => {
    byId('ws-state').textContent = `WebSocket closed | code ${event.code}`;
  });
});

byId('ws-send').addEventListener('click', () => {
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    byId('ws-state').textContent = 'WebSocket: connect before sending';
    return;
  }
  socket.send(`lapui ping ${++socketMessageNumber}`);
});

byId('ws-close').addEventListener('click', () => {
  if (socket) socket.close();
});

byId('toggle-popover').addEventListener('click', () => {
  popoverOpen = !popoverOpen;
  if (popoverOpen) byId('popover').removeAttribute('hidden');
  else byId('popover').setAttribute('hidden', '');
  byId('popover-state').textContent = `Popover ${popoverOpen ? 'open' : 'closed'}`;
  byId('toggle-popover').textContent = `${popoverOpen ? 'Hide' : 'Show'} popover`;
});

byId('start-task').addEventListener('click', () => {
  const task = ++taskNumber;
  let progress = 0;
  byId('task-state').textContent = `Task ${task}: 0%`;
  const advance = () => {
    progress += 25;
    byId('task-state').textContent = `Task ${task}: ${progress}%`;
    if (progress < 100) setTimeout(advance, 80);
    else byId('task-state').textContent = `Task ${task}: complete`;
  };
  setTimeout(advance, 80);
});

byId('inject-error').addEventListener('click', () => {
  setTimeout(() => { throw new Error('network-demo injected async error'); }, 0);
});
