(() => {
  const byId = id => document.getElementById(id);
  const search = byId('search'), rename = byId('rename'), save = byId('save');
  let current = lapui.observe(), selectedId = current.state.files[0].id;
  let draftVersion = 1, dirty = false, invocation = 0, operationId = null;
  const buttons = new Map();
  for (const file of current.state.files) {
    const button = document.createElement('button'); button.id = `open-${file.id}`; button.className = 'file';
    button.addEventListener('click', () => { selectedId = file.id; dirty = false; render(current); });
    byId('file-list').appendChild(button); buttons.set(file.id, button);
  }
  const selected = () => current.state.files.find(file => file.id === selectedId);
  function render(observation) {
    if (observation.version < current.version) return;
    current = observation;
    lapui.batch(() => {
      const query = search.value.toLowerCase();
      for (const file of current.state.files) {
        const button = buttons.get(file.id); button.textContent = file.name;
        button.style.setProperty('display', file.name.toLowerCase().includes(query) ? 'block' : 'none');
        button.style.setProperty('background-color', selectedId === file.id ? '#e8efff' : '#ffffff');
      }
      const file = selected();
      byId('selected').textContent = `已选择：${file.name}`;
      byId('entity-version').textContent = `对象版本 ${file.version} · ${file.size} bytes`;
      byId('app-version').textContent = `应用版本 ${current.version}`;
      if (!dirty) { rename.value = file.name; draftVersion = file.version; }
      save.disabled = !dirty || !rename.value.trim();
    });
  }
  globalThis.__lapui_render = render;
  search.addEventListener('input', () => render(current));
  rename.addEventListener('input', () => { dirty = true; render(current); });
  byId('refresh').addEventListener('click', () => { dirty = false; render(lapui.observe()); byId('rename-status').textContent = '已重新读取当前名称'; });
  save.addEventListener('click', async () => {
    const id = selectedId, name = rename.value, version = draftVersion;
    save.disabled = true;
    try {
      const result = await lapui.invoke('files.rename', {fileId:id, name, expectedFileVersion:version}, {requestId:`ui-rename-${++invocation}`});
      if (selectedId === id && rename.value === name) dirty = false;
      render(result); byId('rename-status').textContent = '名称已保存';
    } catch (error) {
      render(lapui.observe());
      byId('rename-status').textContent = error.code === 'stale_entity' ? '名称已被其他操作者更新。草稿已保留，请重新读取后再编辑。' : `失败：${error.message}`;
    }
  });
  byId('scan').addEventListener('click', async () => {
    byId('scan').disabled = true;
    try {
      const accepted = await lapui.invoke('files.scan', {}, {requestId:`ui-scan-${++invocation}`});
      operationId = accepted.result.operationId; byId('cancel').disabled = false;
      let job = lapui.operation(operationId);
      while (!['completed','failed','cancelled'].includes(job.execution)) {
        byId('scan-status').textContent = `扫描 ${Math.round(job.progress * 100)}% · ${job.message || job.execution}`;
        job = await lapui.waitOperation(operationId, job.revision);
      }
      byId('scan-status').textContent = job.execution === 'completed' ? `扫描完成：${job.output.total} 个示例文件` : job.execution === 'cancelled' ? '扫描已取消' : `扫描失败：${job.error.message}`;
    } catch (error) { byId('scan-status').textContent = `扫描失败：${error.message}`; }
    finally { operationId = null; byId('scan').disabled = false; byId('cancel').disabled = true; }
  });
  byId('cancel').addEventListener('click', () => { if (operationId) lapui.cancelOperation(operationId); });
  byId('settings-toggle').addEventListener('click', () => byId('settings').style.setProperty('display', 'block'));
  byId('settings-close').addEventListener('click', () => byId('settings').style.setProperty('display', 'none'));
  render(current);
})();
