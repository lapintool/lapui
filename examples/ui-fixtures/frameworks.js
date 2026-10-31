// Scenarios over the unchanged, real framework bundles in sibling examples.
(() => {
  const byId = id => document.getElementById(id);
  async function waitFor(predicate, label) {
    const deadline = Date.now() + 2500;
    while (!predicate()) {
      if (Date.now() >= deadline) throw new Error('Condition timed out: ' + label);
      await new Promise(resolve => setTimeout(resolve, 5));
    }
  }
  function fill(target, value) {
    if (typeof lapui !== 'undefined') {
      if (!lapui.fill(target.__ref, value)) throw new Error('Native fill rejected');
    } else {
      // Bypass a framework's own setter, as a user edit would, then send real DOM events.
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(target, value);
      target.dispatchEvent(new Event('input', {bubbles:true}));
      target.dispatchEvent(new Event('change', {bubbles:true}));
    }
  }
  function measure(selectors) {
    return Object.fromEntries(selectors.map(selector => {
      const element = document.querySelector(selector);
      if (!element) throw new Error('Missing measured element: ' + selector);
      const rect = element.getBoundingClientRect();
      return [selector, {x:rect.x, y:rect.y, width:rect.width, height:rect.height}];
    }));
  }
  const frames = () => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  async function vue() {
    await waitFor(() => byId('task-input'), 'Vue mount');
    fill(byId('task-input'), '跨平台中文任务');
    await waitFor(() => !byId('add-task').disabled, 'Vue draft');
    byId('add-task').click();
    await waitFor(() => document.querySelectorAll('.item').length === 3 && byId('task-input').value === '', 'Vue add');
    byId('task-done-2').click();
    await waitFor(() => document.querySelector('.item-name.done'), 'Vue checked state');
    byId('remove-task-1').click();
    await waitFor(() => document.querySelectorAll('.item').length === 2 && !byId('remove-task-1'), 'Vue remove');
    byId('toggle-details').click();
    await waitFor(() => byId('close-details'), 'Vue conditional dialog');
    await frames();
    const states = {
      titles:[...document.querySelectorAll('.item-name')].map(item => item.textContent),
      checked:byId('task-done-2').checked, input:byId('task-input').value,
      status:document.querySelector('.status').textContent,
      dialog:document.querySelector('.dialog').textContent,
    };
    const dialog = document.querySelector('.dialog').getBoundingClientRect();
    return {states, checks:{updated:states.titles.length === 2 && states.titles[1] === '跨平台中文任务', checked:states.checked, inputCleared:states.input === '', dialogCentered:Math.abs(dialog.y + dialog.height / 2 - innerHeight / 2) <= 1},
      metrics:measure(['main','#task-input','.items','.item','.dialog-backdrop','.dialog','#close-details'])};
  }
  async function react() {
    await waitFor(() => byId('react-status')?.textContent === 'React effect completed', 'React effect');
    fill(byId('react-input'), '跨平台中文任务');
    await waitFor(() => !byId('react-add').disabled, 'React controlled input');
    byId('react-add').click();
    await waitFor(() => byId('react-list').children.length === 3 && byId('react-input').value === '', 'React add/reset');
    byId('react-remove-0').click();
    await waitFor(() => byId('react-list').children.length === 2, 'React remove');
    byId('react-details-toggle').click();
    await waitFor(() => byId('react-details'), 'React conditional content');
    await frames();
    const states = {rows:[...byId('react-list').children].map(row => row.textContent), input:byId('react-input').value,
      status:byId('react-status').textContent, details:byId('react-details').textContent, count:byId('react-count').textContent};
    return {states, checks:{updated:states.rows.length === 2 && states.rows[1].includes('跨平台中文任务'), controlledReset:states.input === '', backendUntouched:states.count === 'Count: 0'},
      metrics:measure(['main','#react-input','#react-add','#react-list','#react-details','#react-status'])};
  }
  async function floating(name) {
    await waitFor(() => byId('anchor'), 'Floating UI page');
    byId('anchor').click();
    await waitFor(() => byId('status').value === 'Open', 'Floating UI initial position');
    if (name !== 'floating-center') {
      const previous = byId('coordinates').value;
      byId(name === 'floating-shift' ? 'left-edge' : 'edge').click();
      await waitFor(() => byId('status').value === 'Open' && byId('coordinates').value !== previous, 'Floating UI edge position');
    }
    if (name === 'floating-resize') {
      const previous = byId('coordinates').value;
      byId('resize-anchor').click();
      await waitFor(() => byId('anchor').getBoundingClientRect().width === 160 && byId('coordinates').value !== previous, 'Floating UI autoUpdate');
    }
    await frames();
    await waitFor(() => byId('status').value === 'Open', 'Floating UI final position');
    const states = {status:byId('status').value, placement:byId('placement').value,
      coordinates:byId('coordinates').value, withinStage:byId('bounds').value, computedWidth:byId('style-width').value};
    return {states, checks:{positioned:states.status === 'Open', contained:states.withinStage === 'Yes', middleware: name === 'floating-center' || name === 'floating-shift' || states.placement.startsWith('top')},
      metrics:measure(['main','#stage','#anchor','#popover','#accept'])};
  }
  globalThis.runFrameworkFixture = async name => {
    const output = document.createElement('pre'); output.id = 'fixture-result'; output.style.display = 'none'; document.body.appendChild(output);
    try {
      const result = await (name === 'vue' ? vue() : name === 'react' ? react() : floating(name));
      output.textContent = JSON.stringify({schemaVersion:1, scenario:name, status:'ready', viewport:{width:innerWidth,height:innerHeight},
        layoutViewport:{width:document.documentElement.clientWidth,height:document.documentElement.clientHeight}, ...result});
    } catch (error) {
      output.textContent = JSON.stringify({scenario:name, status:'error', error:String(error.stack || error)});
    }
  };
})();
