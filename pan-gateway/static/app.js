const messagesEl = document.getElementById('messages');
const inputEl = document.getElementById('input');
const sendEl = document.getElementById('send');
const modelSelect = document.getElementById('model-select');

let currentAbort = null;

// Load agent list on startup.
fetch('/v1/agents')
  .then(r => r.json())
  .then(agents => {
    modelSelect.innerHTML = agents.map(a => `<option value="${a}">${a}</option>`).join('');
  });

function addMessage(role, text) {
  const el = document.createElement('div');
  el.className = `message ${role}`;
  el.textContent = text;
  messagesEl.appendChild(el);
  messagesEl.scrollTop = messagesEl.scrollHeight;
  return el;
}

function updateMessage(el, text) {
  el.textContent = text;
  messagesEl.scrollTop = messagesEl.scrollHeight;
}

async function send() {
  const text = inputEl.value.trim();
  if (!text) return;
  inputEl.value = '';
  addMessage('user', text);

  const model = modelSelect.value || 'echo';
  const body = JSON.stringify({
    model,
    messages: [{ role: 'user', content: text }],
    stream: true,
  });

  const assistantMsg = addMessage('assistant', '…');

  try {
    const resp = await fetch('/v1/chat/completions', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body,
    });

    if (!resp.ok) {
      updateMessage(assistantMsg, `Error: ${resp.status}`);
      return;
    }

    const reader = resp.body.getReader();
    const decoder = new TextDecoder();
    let buf = '';

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      const lines = buf.split('\n');
      buf = lines.pop() || '';
      for (const line of lines) {
        if (line.startsWith('data: ')) {
          const data = line.slice(6).trim();
          if (!data) continue;
          try {
            const parsed = JSON.parse(data);
            if (parsed.type === 'token') {
              updateMessage(assistantMsg, parsed.content);
            }
            if (parsed.event === 'done' || data.event === 'done') {
              // Final event — message is complete.
            }
          } catch (_) {
            // Try SSE event format.
            if (line.includes('event: done')) {
              // done event, skip payload
            }
          }
        }
        if (line.startsWith('event: done')) {
          // Consume the following data line.
          continue;
        }
      }
    }
  } catch (err) {
    updateMessage(assistantMsg, `Connection error: ${err}`);
  }
}

sendEl.addEventListener('click', send);
inputEl.addEventListener('keydown', e => {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault();
    send();
  }
});
