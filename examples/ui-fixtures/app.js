(() => {
  const byId = id => document.getElementById(id);
  const names = ['form', 'list', 'scroll', 'cjk', 'popover', 'resize', 'async'];
  const show = name => {
    for (const current of names) byId(current + '-panel').style.display = current === name ? 'block' : 'none';
  };
  const box = element => {
    const rect = element.getBoundingClientRect();
    return {x:rect.x, y:rect.y, width:rect.width, height:rect.height};
  };
  const metrics = ids => Object.fromEntries(['workbench', ...ids].map(id => [id, box(byId(id))]));
  let nextItem = 1;
  function addItem() {
    const id = nextItem++;
    const row = document.createElement('li'); row.id = 'item-' + id; row.className = 'item';
    const label = document.createElement('span'); label.textContent = 'Item ' + id;
    const remove = document.createElement('button'); remove.id = 'remove-' + id; remove.type = 'button'; remove.textContent = 'Remove';
    remove.onclick = () => row.remove(); row.append(label, remove); byId('items').appendChild(row);
  }
  for (let index = 0; index < 3; index++) addItem();
  byId('add-item').onclick = addItem;
  byId('toggle-details').onclick = () => { byId('details').style.display = byId('details').style.display === 'none' ? 'block' : 'none'; };
  byId('form').addEventListener('submit', event => {
    event.preventDefault();
    const data = new FormData(byId('form'));
    byId('form-status').textContent = 'Saved: ' + data.get('title') + ' / ' + data.get('consent');
  });
  for (let index = 1; index <= 12; index++) {
    const row = document.createElement('button'); row.id = 'scroll-' + index; row.type = 'button'; row.className = 'scroll-row'; row.textContent = 'Row ' + index;
    row.onclick = () => { byId('selection').textContent = 'Selected row ' + index; }; byId('scroll-rows').appendChild(row);
  }
  byId('anchor').onclick = () => { byId('popover').style.display = 'block'; };
  byId('accept').onclick = () => { byId('popover').style.display = 'none'; };
  let completeTask = null;
  byId('start-task').onclick = () => {
    byId('task-status').textContent = 'Running';
    Promise.resolve().then(() => setTimeout(() => {
      byId('task-status').textContent = 'Completed'; completeTask?.(); completeTask = null;
    }, 5));
  };
  const scenarios = {
    form() {
      byId('title').value = '中文编辑'; byId('consent').checked = true; byId('form').requestSubmit(byId('save'));
      const states = {value:byId('title').value, checked:byId('consent').checked, status:byId('form-status').textContent};
      return {states, checks:{submitted:states.status === 'Saved: 中文编辑 / on'}, metrics:metrics(['form-panel','title','save','form-status'])};
    },
    list() {
      byId('add-item').click(); byId('remove-1').click(); byId('toggle-details').click();
      const states = {rows:[...byId('items').children].map(row => row.id), details:byId('details').style.display};
      return {states, checks:{updated:states.rows.join(',') === 'item-2,item-3,item-4' && states.details === 'block'}, metrics:metrics(['list-panel','items','item-4','details'])};
    },
    scroll() {
      const port = byId('scroll-port'); port.scrollTo(0, 9999); byId('scroll-12').click();
      const states = {scrollTop:port.scrollTop, clientHeight:port.clientHeight, scrollHeight:port.scrollHeight, selection:byId('selection').textContent};
      return {states, checks:{clamped:states.scrollTop === states.scrollHeight - states.clientHeight, selected:states.selection === 'Selected row 12'}, metrics:metrics(['scroll-panel','scroll-port','scroll-12','selection'])};
    },
    cjk() {
      return {states:{text:byId('cjk-text').textContent}, checks:{wrapped:byId('cjk-text').getBoundingClientRect().height > 24}, metrics:metrics(['cjk-panel','cjk-text'])};
    },
    popover() {
      byId('anchor').click();
      const states = {display:byId('popover').style.display};
      return {states, checks:{opened:states.display === 'block'}, metrics:metrics(['popover-panel','stage','anchor','popover','accept'])};
    },
    resize() {
      const a = box(byId('column-a')), b = box(byId('column-b'));
      const stacked = b.y > a.y;
      return {states:{stacked}, checks:{responsive:stacked === (innerWidth <= 700)}, metrics:metrics(['resize-panel','columns','column-a','column-b'])};
    },
    async async() {
      const completed = new Promise(resolve => { completeTask = resolve; }); byId('start-task').click(); await completed;
      const states = {status:byId('task-status').textContent};
      return {states, checks:{completed:states.status === 'Completed'}, metrics:metrics(['async-panel','start-task','task-status'])};
    }
  };
  globalThis.runFixture = async name => {
    try {
      if (!scenarios[name]) throw new Error('Unknown scenario: ' + name);
      show(name);
      const result = await scenarios[name]();
      byId('fixture-result').textContent = JSON.stringify({schemaVersion:1, scenario:name, status:'ready', viewport:{width:innerWidth,height:innerHeight}, ...result});
    } catch (error) {
      byId('fixture-result').textContent = JSON.stringify({scenario:name,status:'error',error:String(error.stack || error)});
    }
  };
  for (const button of document.querySelectorAll('[data-case]')) button.onclick = () => { runFixture(button.getAttribute('data-case')); };
  show('form');
})();
