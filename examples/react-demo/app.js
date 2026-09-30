import { createElement as h, useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';

function App() {
  const [draft, setDraft] = useState('');
  const [items, setItems] = useState(['Local React DOM', '共享 Rust 动作']);
  const [details, setDetails] = useState(false);
  const [status, setStatus] = useState('Mounting…');
  const [count, setCount] = useState(0);
  const [busy, setBusy] = useState(false);
  useEffect(() => { setStatus('React effect completed'); }, []);
  const add = () => {
    const title = draft.trim();
    if (!title) return;
    setItems(previous => [...previous, title]);
    setDraft('');
    setStatus(`Added: ${title}`);
  };
  const increment = async () => {
    setBusy(true);
    try {
      const state = await lapui.invoke('counter.increment');
      setCount(state.count);
      setStatus(`Rust version: ${state.version}`);
    } catch (error) { setStatus(String(error.message || error)); }
    finally { setBusy(false); }
  };
  return h('main', null,
    h('h1', null, 'React on Lapui'),
    h('label', { htmlFor: 'react-input' }, 'New item'),
    h('input', { id: 'react-input', type: 'text', value: draft, onChange: event => setDraft(event.target.value) }),
    h('button', { id: 'react-add', disabled: !draft.trim(), onClick: add }, 'Add item'),
    h('ul', { id: 'react-list' }, items.map((title, index) =>
      h('li', { key: title + index }, title,
        h('button', { id: `react-remove-${index}`, onClick: () => setItems(previous => previous.filter((_, i) => i !== index)) }, 'Remove')))),
    h('button', { id: 'react-details-toggle', onClick: () => setDetails(previous => !previous) }, 'Toggle details'),
    details && h('section', { id: 'react-details', className: 'panel' }, 'Conditional React content'),
    h('p', { id: 'react-count', role: 'status' }, `Count: ${count}`),
    h('button', { id: 'react-increment', disabled: busy, onClick: increment }, 'Increment through Rust'),
    h('p', { id: 'react-status', role: 'status' }, status));
}

const root = createRoot(document.getElementById('app'));
root.render(h(App));
