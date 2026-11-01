const status = document.getElementById('status');

let presenceActive = false;
let presenceTimer;
let presenceSocket;

function retryPresence(socket) {
  if (presenceSocket !== socket) return;
  presenceSocket = null;
  socket.close();
  clearTimeout(presenceTimer);
  if (presenceActive) presenceTimer = setTimeout(connectPresence, 500);
}

function connectPresence() {
  if (!presenceActive) return;
  clearTimeout(presenceTimer);
  const previous = presenceSocket;
  presenceSocket = null;
  previous?.close();
  const url = new URL('presence', location.href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  const socket = new WebSocket(url.href);
  presenceSocket = socket;
  presenceTimer = setTimeout(() => retryPresence(socket), 8000);
  socket.addEventListener('open', () => {
    if (presenceSocket === socket) clearTimeout(presenceTimer);
  });
  socket.addEventListener('close', () => retryPresence(socket));
  socket.addEventListener('error', () => retryPresence(socket));
}

window.addEventListener('pageshow', () => {
  presenceActive = true;
  connectPresence();
});
window.addEventListener('pagehide', () => {
  presenceActive = false;
  clearTimeout(presenceTimer);
  const socket = presenceSocket;
  presenceSocket = null;
  socket?.close(1000);
});
window.addEventListener('online', connectPresence);

document.querySelectorAll('[data-copy]').forEach(button => {
  button.addEventListener('click', async () => {
    const field = document.getElementById(button.dataset.copy);
    try {
      if (navigator.clipboard && window.isSecureContext) {
        await navigator.clipboard.writeText(field.value);
      } else {
        field.focus();
        field.select();
        field.setSelectionRange(0, field.value.length);
        if (!document.execCommand('copy')) throw new Error('Select and copy the text.');
      }
      status.textContent = 'Copied';
    } catch (error) {
      field.focus();
      field.select();
      status.textContent = 'Select and copy the text.';
    }
  });
});

async function send(path, options) {
  const response = await fetch(path, options);
  if (!response.ok) throw new Error(await response.text() || 'Could not send.');
}

async function submit(form, action) {
  const button = form.querySelector('button');
  button.disabled = true;
  status.textContent = 'Sending…';
  try {
    await action();
    form.reset();
    status.textContent = 'Sent';
  } catch (error) {
    status.textContent = error.message || 'Could not send.';
  } finally {
    button.disabled = false;
  }
}

document.getElementById('files').addEventListener('submit', event => {
  event.preventDefault();
  submit(event.currentTarget, async () => {
    for (const file of document.getElementById('upload').files) {
      if (file.size > 1024 ** 3) throw new Error('File exceeds 1 GiB.');
      const data = new FormData();
      data.append('file', file);
      await send('upload', { method: 'POST', body: data });
    }
  });
});

document.getElementById('text').addEventListener('submit', event => {
  event.preventDefault();
  submit(event.currentTarget, () => send('text', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text: document.getElementById('message').value }),
  }));
});
