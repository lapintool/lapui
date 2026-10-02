(() => {
  const byId = id => document.getElementById(id);
  const search = byId('search'), note = byId('note'), save = byId('save');
  let current = lapui.observe();
  let selectedId = current.state.files[0]?.id ?? null;
  let draftVersion = 0, dirty = false, invocation = 0;
  const buttons = new Map();
  function createButton(file) {
    const button = document.createElement('button');
    button.id = `open-${file.id}`; button.className = 'file';
    button.addEventListener('click', () => { selectedId = file.id; dirty = false; render(current); });
    byId('file-list').appendChild(button); buttons.set(file.id, button);
  }
  const selected = () => current.state.files.find(file => file.id === selectedId) ?? null;
  function render(observation) {
    if (observation.version < current.version) return;
    current = observation;
    for (const file of current.state.files) if (!buttons.has(file.id)) createButton(file);
    for (const [id, button] of buttons) {
      const file = current.state.files.find(item => item.id === id);
      if (!file) { button.remove(); buttons.delete(id); }
    }
    const query = search.value.toLowerCase();
    for (const file of current.state.files) {
      const button = buttons.get(file.id);
      button.textContent = file.name;
      button.style.setProperty('display', file.name.toLowerCase().includes(query) ? 'block' : 'none');
      button.style.setProperty('background-color', selectedId === file.id ? '#e8efff' : '#ffffff');
    }
    const file = selected();
    save.disabled = !dirty || !file;
    byId('selected').textContent = file ? `已选择：${file.name}` : '目录中没有可索引的普通文件';
    byId('entity-version').textContent = file ? `对象版本 ${file.version} · ${file.size} bytes` : '';
    byId('count').textContent = `索引 ${current.state.files.length} 个文件 · 目录 ${current.state.directoryName}`;
    byId('app-version').textContent = `应用状态版本 ${current.version}`;
    if (file && !dirty) { note.value = file.metadata.note; draftVersion = file.version; }
  }
  globalThis.__lapui_render = render;
  async function followSharedState() {
    let page = await lapui.changes.subscribe({scope:'state', limit:32});
    let cursor = page.cursor;
    // Close the gap between the first observation and the subscription checkpoint.
    render(lapui.observe());
    while (true) {
      page = await lapui.changes.subscribe({scope:'state', cursor, limit:32, waitMs:1000});
      cursor = page.cursor;
      if (page.resyncRequired || page.records.some(record => record.kind === 'state_changed')) {
        render(lapui.observe());
      }
    }
  }
  followSharedState().catch(error => { byId('status').textContent = `状态同步失败：${error.message}`; });
  search.addEventListener('input', () => render(current));
  note.addEventListener('input', () => { dirty = true; render(current); });
  save.addEventListener('click', async () => {
    const file = selected();
    if (!file) return;
    const id = selectedId, text = note.value, version = draftVersion;
    save.disabled = true;
    try {
      const result = await lapui.invoke('local_files.metadata.update', {
        fileId:id, note:text, expectedFileVersion:version
      }, {requestId:`ui-metadata-${++invocation}`});
      if (selectedId === id && note.value === text) dirty = false;
      render(result); byId('status').textContent = 'Lapui 备注已保存';
    } catch (error) {
      render(lapui.observe());
      byId('status').textContent = error.code === 'stale_entity'
        ? '此文件的备注已在其他位置更新。草稿已保留，请重新索引后再编辑。'
        : `保存失败：${error.message}`;
    }
  });
  byId('refresh').addEventListener('click', async () => {
    const button = byId('refresh'); button.disabled = true;
    try {
      const result = await lapui.invoke('local_files.refresh', {}, {requestId:`ui-refresh-${++invocation}`});
      dirty = false;
      if (!current.state.files.some(file => file.id === selectedId)) selectedId = result.state.files[0]?.id ?? null;
      render(result); byId('status').textContent = `重新索引完成：${result.state.files.length} 个文件`;
    } catch (error) { byId('status').textContent = `索引失败：${error.message}`; }
    finally { button.disabled = false; }
  });
  render(current);
})();
