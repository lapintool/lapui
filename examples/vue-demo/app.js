import { createApp, h, ref } from 'vue';

const App = {
  setup() {
    const draft = ref('');
    const status = ref('就绪 · 使用 Vue 3 runtime-dom 更新真实 Blitz DOM');
    const detailsOpen = ref(false);
    let nextId = 3;
    const tasks = ref([
      { id: 1, title: '探索 QuickJS 与 Blitz 的 DOM 桥接', done: false },
      { id: 2, title: '验证 CSS class 和响应式列表更新', done: false }
    ]);

    const addTask = () => {
      const title = draft.value.trim();
      if (!title) return;
      const id = nextId++;
      tasks.value.push({ id, title });
      draft.value = '';
      status.value = `已添加任务：${title}`;
    };

    const removeTask = task => {
      tasks.value = tasks.value.filter(item => item.id !== task.id);
      status.value = `已删除任务：${task.title}`;
    };

    return () => h('main', { class: 'board' }, [
      h('div', { class: 'eyebrow' }, 'Local UI · Vue 3'),
      h('h1', '任务面板'),
      h('p', { class: 'subtitle' }, '用熟悉的前端方式编写本地桌面界面。'),
      h('form', {
        class: 'entry',
        onSubmit: event => { event.preventDefault(); addTask(); }
      }, [
        h('label', { for: 'task-input' }, '新任务'),
        h('input', {
          id: 'task-input',
          placeholder: '输入任务名称…',
          value: draft.value,
          onInput: event => { draft.value = event.target.value; }
        }),
        h('button', { id: 'add-task', type: 'button', disabled: !draft.value.trim(), onClick: addTask }, '添加任务')
      ]),
      h('ul', { class: 'items', 'aria-label': '任务列表' }, tasks.value.map(task =>
        h('li', { class: 'item', key: task.id }, [
        h('input', {
          id: `task-done-${task.id}`,
          class: 'task-check',
          type: 'checkbox',
          checked: task.done,
          'aria-label': `完成 ${task.title}`,
          onChange: () => { task.done = !task.done; }
        }),
        h('span', { class: task.done ? 'item-name done' : 'item-name' }, task.title),
          h('button', {
            class: 'remove',
            id: `remove-task-${task.id}`,
            type: 'button',
            'aria-label': `删除 ${task.title}`,
            onClick: () => removeTask(task)
          }, '删除')
        ])
      )),
      h('div', { class: 'actions' }, [
        h('button', {
          id: 'toggle-details',
          class: 'secondary',
          type: 'button',
          'aria-expanded': String(detailsOpen.value),
          onClick: () => { detailsOpen.value = !detailsOpen.value; }
        }, detailsOpen.value ? '关闭说明' : '打开说明'),
        h('button', {
          id: 'clear-tasks',
          class: 'secondary',
          type: 'button',
          onClick: () => { tasks.value = []; status.value = '列表已清空'; }
        }, '清空列表')
      ]),
      h('p', { class: 'status', role: 'status', 'aria-live': 'polite' }, status.value),
      detailsOpen.value ? h('div', { class: 'dialog-backdrop' }, [
        h('section', { class: 'dialog', role: 'dialog', 'aria-modal': 'true', 'aria-label': 'Lapui 说明' }, [
          h('h2', '一个本地界面'),
          h('p', '此示例使用 Vue 3 的响应式状态、列表 diff、事件监听器和弹层节点插入。'),
          h('button', { id: 'close-details', type: 'button', onClick: () => { detailsOpen.value = false; } }, '完成')
        ])
      ]) : null
    ]);
  }
};

createApp(App).mount('#app');
