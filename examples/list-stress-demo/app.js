const query = document.getElementById('query');
const list = document.getElementById('rows');
const result = document.getElementById('result');
const viewport = document.getElementById('viewport');
const entries = Array.from({ length: 1000 }, (_, index) => ({
  id: index + 1,
  name: 'Entry ' + String(index + 1).padStart(4, '0') + ' — deterministic list fixture',
}));
const rowHeight = 28;
const windowSize = 100;
let filtered = entries;

function renderWindow() {
  const first = Math.max(0, Math.min(
    filtered.length - windowSize,
    Math.floor(viewport.scrollTop / rowHeight) - 8,
  ));
  const last = Math.min(filtered.length, first + windowSize);
  const fragment = document.createDocumentFragment();
  for (let index = first; index < last; index++) {
    const entry = filtered[index];
    const row = document.createElement('li');
    row.id = 'entry-' + String(entry.id).padStart(4, '0');
    row.className = 'row';
    row.setAttribute('aria-posinset', String(index + 1));
    row.setAttribute('aria-setsize', String(filtered.length));
    row.style.top = `${index * rowHeight}px`;
    row.textContent = entry.name;
    fragment.appendChild(row);
  }
  list.replaceChildren(fragment);
  list.style.height = `${Math.max(viewport.clientHeight, filtered.length * rowHeight)}px`;
  result.textContent = `${filtered.length} / ${entries.length} entries · ${last - first} rows rendered`;
}

function update() {
  const term = query.value.trim().toLowerCase();
  filtered = term ? entries.filter(entry => entry.name.toLowerCase().includes(term)) : entries;
  viewport.scrollTop = 0;
  renderWindow();
}

query.addEventListener('input', update);
document.getElementById('clear').addEventListener('click', () => {
  query.value = '';
  update();
});
viewport.addEventListener('scroll', renderWindow);
renderWindow();
