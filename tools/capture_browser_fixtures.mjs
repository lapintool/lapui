// Development-only Chromium reference capture. Requires Node.js 22+; no packages.
import {spawn} from 'node:child_process';
import {readFile, writeFile, mkdir, mkdtemp, rm} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const fixture = path.join(root, 'examples', 'ui-fixtures');
const options = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  if (!process.argv[index + 1]) throw new Error('Each option requires a value');
  options.set(process.argv[index], process.argv[index + 1]);
}
if (!options.has('--browser')) throw new Error('--browser is required');
const output = path.resolve(options.get('--out') || path.join(root, 'target', 'fixture-comparison'));
await mkdir(output, {recursive:true});
const profile = await mkdtemp(path.join(output, 'browser-profile-'));
const child = spawn(options.get('--browser'), [
  '--headless=new', '--no-first-run', '--no-default-browser-check',
  '--disable-background-networking', '--disable-extensions', '--disable-gpu',
  '--force-color-profile=srgb', '--remote-debugging-address=127.0.0.1',
  '--remote-debugging-port=0', '--user-data-dir=' + profile, 'about:blank',
], {stdio:['ignore', 'ignore', 'pipe'], windowsHide:true});
let stderr = '', launchError = null;
child.stderr.on('data', data => { stderr = (stderr + data).slice(-4096); });
child.on('error', error => { launchError = error; });
const delay = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));

class Protocol {
  constructor(socket) {
    this.socket = socket; this.nextId = 1; this.pending = new Map();
    socket.addEventListener('message', event => {
      const message = JSON.parse(event.data);
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id); clearTimeout(pending.timer);
      if (message.error) pending.reject(new Error(JSON.stringify(message.error)));
      else pending.resolve(message.result);
    });
    socket.addEventListener('close', () => {
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timer); pending.reject(new Error('Browser connection closed'));
      }
      this.pending.clear();
    });
  }
  call(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(method + ' timed out')); }, 15000);
      this.pending.set(id, {resolve, reject, timer});
      this.socket.send(JSON.stringify({id, method, params}));
    });
  }
  async evaluate(expression) {
    const result = await this.call('Runtime.evaluate', {expression, returnByValue:true, awaitPromise:true});
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  }
}

let protocol;
try {
  const deadline = Date.now() + 15000;
  let port;
  while (Date.now() < deadline) {
    if (launchError) throw launchError;
    if (child.exitCode !== null) throw new Error('Browser exited: ' + stderr);
    try { port = Number((await readFile(path.join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]); } catch {}
    if (port > 0) break;
    await delay(25);
  }
  if (!port) throw new Error('Browser debugging port unavailable: ' + stderr);
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`, {signal:AbortSignal.timeout(5000)})).json();
  const target = targets.find(item => item.type === 'page');
  if (!target) throw new Error('No reference page target');
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { socket.close(); reject(new Error('Browser socket did not open')); }, 5000);
    socket.addEventListener('open', () => { clearTimeout(timer); resolve(); }, {once:true});
    socket.addEventListener('error', error => { clearTimeout(timer); reject(error); }, {once:true});
  });
  protocol = new Protocol(socket);
  await protocol.call('Page.enable');
  const version = await protocol.call('Browser.getVersion');
  const manifest = JSON.parse(await readFile(path.join(fixture, 'manifest.json'), 'utf8'));
  for (const viewport of manifest.viewports) {
    const {width, height} = viewport;
    const directory = path.join(output, `${width}x${height}`);
    await mkdir(directory, {recursive:true});
    await protocol.call('Emulation.setDeviceMetricsOverride', {width, height, deviceScaleFactor:1, mobile:false});
    for (const name of manifest.scenarios) {
      const url = pathToFileURL(path.join(fixture, 'index.html'));
      url.searchParams.set('fixture', name);
      url.searchParams.set('viewport', `${width}x${height}`);
      await protocol.call('Page.navigate', {url:url.href});
      const readyDeadline = Date.now() + 5000;
      let loaded = false;
      while (Date.now() < readyDeadline) {
        loaded = await protocol.evaluate(`location.href === ${JSON.stringify(url.href)} && document.readyState === 'complete' && typeof runFixture === 'function'`);
        if (loaded) break;
        await delay(10);
      }
      if (!loaded) throw new Error('Fixture document did not load: ' + name);
      const result = await protocol.evaluate(`runFixture(${JSON.stringify(name)}).then(() => JSON.parse(document.getElementById('fixture-result').textContent))`);
      await writeFile(path.join(directory, name + '-browser.json'), JSON.stringify(result, null, 2));
      if (result.status !== 'ready') throw new Error('Fixture failed: ' + JSON.stringify(result));
      if (result.viewport.width !== width || result.viewport.height !== height) throw new Error('Unexpected CSS viewport');
      const screenshot = await protocol.call('Page.captureScreenshot', {format:'png', captureBeyondViewport:false, fromSurface:true});
      const pixels = Buffer.from(screenshot.data, 'base64');
      if (pixels.readUInt32BE(16) !== width || pixels.readUInt32BE(20) !== height) throw new Error('PNG dimensions do not match the measured viewport');
      await writeFile(path.join(directory, name + '-browser.png'), pixels);
    }
  }
  await writeFile(path.join(output, 'browser-metadata.json'), JSON.stringify({
    ...version, nodeVersion:process.version, capture:'CDP Emulation.setDeviceMetricsOverride + Page.captureScreenshot', deviceScaleFactor:1,
  }, null, 2));
} finally {
  if (protocol) {
    try { await protocol.call('Browser.close'); } catch {}
    protocol.socket.close();
  }
  const exitDeadline = Date.now() + 3000;
  while (child.exitCode === null && !launchError && Date.now() < exitDeadline) await delay(25);
  if (child.exitCode === null && !launchError) child.kill();
  // This is the unique profile created above, always under the selected output.
  if (path.dirname(profile) === output) await rm(profile, {recursive:true, force:true, maxRetries:3, retryDelay:100}).catch(() => {});
}
