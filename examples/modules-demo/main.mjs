import { formatCount } from './counter.mjs';

const greeting = await fetch('./greeting.json').then(response => response.json());
const status = document.getElementById('module-status');
const count = document.getElementById('module-count');
const button = document.getElementById('module-increment');

status.textContent = greeting.message;
status.setAttribute('data-module-url', import.meta.url);
button.disabled = false;

button.addEventListener('click', async () => {
  button.disabled = true;
  try {
    const { increment } = await import('./actions.mjs');
    const state = await increment();
    count.textContent = formatCount(state.count);
    status.textContent = `Rust action completed at version ${state.version}`;
  } catch (error) {
    status.textContent = String(error.message || error);
  } finally {
    button.disabled = false;
  }
});
