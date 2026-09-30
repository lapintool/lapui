const count = document.getElementById('count');
const button = document.getElementById('increment');
const status = document.getElementById('status');

let renderedVersion = 0;
globalThis.__lapui_render = observation => {
  if (observation.version < renderedVersion) return;
  renderedVersion = observation.version;
  lapui.batch(() => {
    count.textContent = String(observation.count);
    status.textContent = `状态版本 ${observation.version}`;
  });
};

button.addEventListener('click', () => {
  lapui.invoke('counter.increment', {}).then(
    observation => __lapui_render(observation),
    error => { status.textContent = `失败: ${error.message}`; }
  );
});

Promise.resolve().then(() => {
  button.style.setProperty('background-color', '#3458d4');
});

__lapui_render(lapui.observe());
