const byId = id => document.getElementById(id);
let pending = null;
let duration = 1200;
let progress = 0;
let startTime = null;
let count = 0;

function measure() {
  const marker = byId('dot').getBoundingClientRect();
  const track = byId('track').getBoundingClientRect();
  byId('geometry').value = marker.x.toFixed(1) + ' / ' + track.width.toFixed(1);
}

function display(phase) {
  byId('phase').value = phase;
  byId('progress').value = Math.round(progress * 100) + '%';
  byId('frames').value = String(count);
  byId('pause').disabled = phase !== 'Running';
  byId('resume').disabled = phase !== 'Paused';
  byId('start').disabled = phase === 'Running';
}

function tick(timestamp) {
  pending = null;
  if (startTime === null) startTime = timestamp - progress * duration;
  progress = Math.min(1, (timestamp - startTime) / duration);
  count++;
  byId('dot').style.left = (progress * 80) + '%';
  measure();
  display(progress === 1 ? 'Complete' : 'Running');
  if (progress < 1) pending = requestAnimationFrame(tick);
}

function stop() {
  if (pending !== null) cancelAnimationFrame(pending);
  pending = null;
  startTime = null;
}

byId('start').onclick = () => {
  const value = Number(byId('duration').value);
  if (!Number.isFinite(value) || value < 100 || value > 5000) {
    display('Enter a duration from 100 to 5000 ms');
    return;
  }
  stop();
  duration = value;
  progress = 0;
  count = 0;
  display('Running');
  pending = requestAnimationFrame(tick);
};
byId('pause').onclick = () => { stop(); display('Paused'); };
byId('resume').onclick = () => { startTime = null; display('Running'); pending = requestAnimationFrame(tick); };
byId('reset').onclick = () => {
  stop();
  progress = 0;
  count = 0;
  byId('dot').style.left = '0';
  display('Ready');
  requestAnimationFrame(measure);
};
requestAnimationFrame(measure);
