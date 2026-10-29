import { spawn, fork, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { watch as watchFiles } from 'node:fs';
import { access, cp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const config = JSON.parse(await readFile(join(root, 'lapui.config.json'), 'utf8'));
const manifest = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
const [mode, ...options] = process.argv.slice(2);
const settings = {
  frontend: process.env.LAPUI_FRONTEND_DIR || config.frontendDirectory,
  renderer: process.env.LAPUI_RENDERER || config.renderer,
  toolchain: process.env.LAPUI_RUST_TOOLCHAIN || config.rustToolchain,
};
for (let index = 0; index < options.length; index++) {
  if (options[index] === '--') continue;
  const key = { '--frontend': 'frontend', '--renderer': 'renderer', '--toolchain': 'toolchain' }[options[index]];
  if (!key || !options[index + 1]) throw new Error('Unknown or incomplete option: ' + options[index]);
  settings[key] = options[++index];
}
if (!['cpu', 'gpu'].includes(settings.renderer)) throw new Error('Renderer must be cpu or gpu.');
const frontend = resolve(root, settings.frontend);
const output = join(root, 'target', 'desktop');
const distribution = join(root, 'dist');
const platform = (process.platform === 'win32' ? 'windows' : process.platform) + '-' + process.arch;
const packageDirectory = join(distribution, 'lapui-' + platform);
const binaryName = process.platform === 'win32' ? 'lapui.exe' : 'lapui';
const children = new Set();
let closing = false;
const log = (message) => console.log('[lapui] ' + message);
const exists = async (path) => access(path).then(() => true, () => false);
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
const cargo = (...args) => ['cargo', ['+' + settings.toolchain, ...args]];

function child(command, args, options = {}) {
  log(command + ' ' + args.join(' '));
  const processChild = spawn(command, args, {
    cwd: root, env: process.env, windowsHide: true, stdio: 'inherit', ...options,
  });
  children.add(processChild);
  processChild.once('close', () => children.delete(processChild));
  return processChild;
}

function finished(processChild, label) {
  return new Promise((done, reject) => {
    processChild.once('error', reject);
    processChild.once('close', (code, signal) => {
      if (closing || code === 0) done();
      else reject(new Error(label + ' failed (' + (signal || code) + ').'));
    });
  });
}

async function run(command, args, options) {
  await finished(child(command, args, options), command);
}

async function stop(processChild) {
  if (!processChild?.pid || processChild.exitCode !== null || processChild.signalCode !== null) return;
  const ended = once(processChild, 'close').catch(() => {});
  if (process.platform === 'win32') {
    const killer = spawn('taskkill.exe', ['/PID', String(processChild.pid), '/T', '/F'], {
      windowsHide: true, stdio: 'ignore',
    });
    await once(killer, 'close').catch(() => {});
  } else {
    processChild.kill('SIGTERM');
  }
  await Promise.race([ended, pause(3000)]);
}

async function cleanup() {
  if (closing) return;
  closing = true;
  await Promise.all([...children].map(stop));
}
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, () => { void cleanup().finally(() => process.exit(0)); });
}

function python() {
  if (process.env.PYTHON3) return;
  const executable = process.platform === 'win32' ? 'py' : 'python3';
  const args = process.platform === 'win32' ? ['-3'] : [];
  process.env.PYTHON3 = execFileSync(executable, [...args, '-c', 'import sys; print(sys.executable)'], {
    encoding: 'utf8', windowsHide: true,
  }).trim();
}

async function frontendCheck() {
  if (!await exists(join(frontend, 'package.json'))) {
    throw new Error('Frontend checkout not found: ' + frontend + '. Set LAPUI_FRONTEND_DIR or --frontend.');
  }
  if (!await exists(join(frontend, 'node_modules', 'vite', 'package.json'))) {
    throw new Error('Frontend dependencies are missing. Run pnpm --dir "' + frontend + '" install first.');
  }
  if (!process.env.npm_execpath) throw new Error('Start this workflow through pnpm.');
}

async function typecheck() {
  await run(process.execPath, [process.env.npm_execpath, '--dir', frontend, 'run', 'typecheck']);
}

function frontendWorker(directory, buildMode) {
  const worker = fork(join(root, 'scripts', 'frontend-worker.mjs'), [frontend, directory, buildMode], {
    cwd: frontend, env: process.env, stdio: ['ignore', 'inherit', 'inherit', 'ipc'], windowsHide: true,
  });
  children.add(worker);
  worker.once('close', () => children.delete(worker));
  return worker;
}

async function dev() {
  const appDirectory = join(output, 'dev', 'app');
  await mkdir(appDirectory, { recursive: true });
  let app;
  let address;
  let restarting = false;
  let rebuilding = false;
  let requested = false;
  let debounce;
  const watchers = [];
  const worker = frontendWorker(appDirectory, 'dev');
  const firstBuild = new Promise((done, reject) => {
    worker.once('error', reject);
    worker.once('close', (code) => reject(new Error('Frontend watcher exited (' + code + ').')));
    const ready = (message) => {
      if (message.type === 'built') { worker.off('message', ready); done(); }
      if (message.type === 'build-error') log('Frontend has errors; waiting for a successful build.');
    };
    worker.on('message', ready);
  });
  let reload = Promise.resolve();
  worker.on('message', (message) => {
    if (message.type !== 'built' || !address || restarting || closing) return;
    const current = address;
    reload = reload.then(async () => {
      if (address !== current || restarting || closing) return;
      await run(join(root, 'target', 'debug', binaryName), ['client', current, 'reload']);
    }).catch((error) => console.error('[lapui] Reload: ' + error.message));
  });
  worker.on('close', () => {
    if (!closing) void cleanup().finally(() => process.exit(1));
  });

  function launch() {
    address = undefined;
    app = child(join(root, 'target', 'debug', binaryName), [
      '--html', join(appDirectory, 'index.html'), '--renderer', settings.renderer, '--debug-trace',
    ], { stdio: ['inherit', 'pipe', 'inherit'] });
    let pendingOutput = '';
    app.stdout.on('data', (chunk) => {
      process.stdout.write(chunk);
      pendingOutput = (pendingOutput + chunk.toString()).slice(-4096);
      const match = pendingOutput.match(/LAPUI_CONTROL=(127\.0\.0\.1:\d+)/);
      if (match) address = match[1];
    });
    app.once('error', (error) => {
      console.error(error);
      void cleanup().finally(() => process.exit(1));
    });
    app.once('close', (code) => {
      address = undefined;
      if (!restarting && !closing) void cleanup().finally(() => process.exit(code === 0 ? 0 : 1));
    });
    log('Frontend edits reload the page; Rust edits rebuild/restart the window. Ctrl+C stops both.');
  }

  async function rebuild() {
    requested = true;
    if (rebuilding) return;
    rebuilding = true;
    restarting = true;
    await stop(app);
    address = undefined;
    while (requested && !closing) {
      requested = false;
      try {
        await run(...cargo('build', '--locked'));
        if (!requested && !closing) {
          restarting = false;
          launch();
        }
      } catch (error) {
        console.error('[lapui] ' + error.message + ' Fix the Rust source and save to retry.');
      }
    }
    rebuilding = false;
  }
  const changed = () => {
    clearTimeout(debounce);
    debounce = setTimeout(() => { void rebuild(); }, 500);
  };
  try {
    await Promise.all([typecheck(), firstBuild, run(...cargo('build', '--locked'))]);
    if (closing) return;
    for (const directory of ['src', 'ui', 'vendor/taffy/src']) {
      watchers.push(watchFiles(join(root, directory), { recursive: true }, changed));
    }
    for (const filename of ['Cargo.toml', 'Cargo.lock', 'build.rs']) {
      watchers.push(watchFiles(join(root, filename), changed));
    }
    launch();
    await new Promise((done) => worker.once('close', done));
  } finally {
    clearTimeout(debounce);
    for (const watcher of watchers) watcher.close();
  }
}

async function walk(directory) {
  const paths = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) paths.push(...await walk(path));
    else if (entry.isFile()) paths.push(path);
    else throw new Error('Unexpected linked resource in package: ' + path);
  }
  return paths.sort();
}

async function release() {
  const appDirectory = join(output, 'release', 'app');
  await mkdir(output, { recursive: true });
  await typecheck();
  const worker = frontendWorker(appDirectory, 'release');
  await Promise.all([
    finished(worker, 'Frontend build'),
    run(...cargo('build', '--release', '--locked')),
  ]);
  if (closing) return;
  // Remove only this workflow's marked output inside the fixed dist root.
  const rel = relative(distribution, packageDirectory);
  if (!rel || rel.startsWith('..' + sep) || rel === '..') throw new Error('Unsafe package output.');
  if (await exists(packageDirectory)) {
    if (!await exists(join(packageDirectory, 'BUILD-MANIFEST.json'))) {
      throw new Error('Refusing to replace an unmarked directory: ' + packageDirectory);
    }
    await rm(packageDirectory, { recursive: true });
  }
  await mkdir(packageDirectory, { recursive: true });
  await writeFile(join(packageDirectory, 'BUILD-MANIFEST.json'), '{"status":"building"}\n');
  await cp(join(root, 'target', 'release', binaryName), join(packageDirectory, binaryName));
  await cp(appDirectory, join(packageDirectory, 'app'), { recursive: true });
  for (const name of ['LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY.md', 'prerequisites']) {
    await cp(join(root, name), join(packageDirectory, name), { recursive: true });
  }
  await cp(join(root, 'licenses'), join(packageDirectory, 'licenses'), { recursive: true });
  await cp(join(root, 'vendor/taffy/LICENSE'), join(packageDirectory, 'licenses', 'taffy-LICENSE'));
  if (await exists(join(frontend, 'LICENSE'))) {
    await cp(join(frontend, 'LICENSE'), join(packageDirectory, 'licenses', 'frontend-LICENSE'));
  }
  await cp(join(root, 'Cargo.lock'), join(packageDirectory, 'Cargo.lock'));
  const files = await Promise.all((await walk(packageDirectory))
    .filter((path) => path !== join(packageDirectory, 'BUILD-MANIFEST.json'))
    .map(async (path) => {
      const bytes = await readFile(path);
      return {
        path: relative(packageDirectory, path).split(sep).join('/'),
        bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'),
      };
    }));
  const git = (args, cwd = root) => {
    try { return execFileSync('git', args, { cwd, encoding: 'utf8', windowsHide: true }).trim(); }
    catch { return null; }
  };
  await writeFile(join(packageDirectory, 'BUILD-MANIFEST.json'), JSON.stringify({
    version: manifest.version, platform, status: 'experimental-preview',
    sourceCommit: git(['rev-parse', 'HEAD']),
    sourceDirty: Boolean(git(['status', '--porcelain', '--untracked-files=normal'])),
    frontendCommit: git(['rev-parse', 'HEAD'], frontend),
    frontendDirty: Boolean(git(['status', '--porcelain'], frontend)),
    rustToolchain: settings.toolchain, createdAt: new Date().toISOString(), files,
  }, null, 2) + '\n');
  log('Release ready: ' + join(packageDirectory, binaryName));
  log('Keep the app resource directory beside the executable when copying this release.');
}

async function findNsis() {
  const paths = [
    process.env.NSIS_EXE,
    join(process.env['ProgramFiles(x86)'] || 'C:/Program Files (x86)', 'NSIS', 'makensis.exe'),
    join(process.env.ProgramFiles || 'C:/Program Files', 'NSIS', 'makensis.exe'),
  ].filter(Boolean);
  for (const path of paths) if (await exists(path)) return path;
  try {
    return execFileSync('where.exe', ['makensis.exe'], { encoding: 'utf8', windowsHide: true })
      .trim().split(/\r?\n/)[0];
  } catch { return null; }
}

async function setupInstaller() {
  if (process.platform !== 'win32') throw new Error('Windows installer builds require Windows.');
  let compiler = await findNsis();
  if (!compiler) {
    await run('winget.exe', [
      'install', '--id', 'NSIS.NSIS', '--exact', '--version', '3.13', '--source', 'winget',
      '--silent', '--disable-interactivity', '--accept-source-agreements', '--accept-package-agreements',
    ]);
    compiler = await findNsis();
  }
  if (!compiler) throw new Error('NSIS not found. Set NSIS_EXE to makensis.exe or install NSIS 3.');
  log('Installer compiler: ' + compiler);
  return compiler;
}

const nsisString = (text) => text.replaceAll('$', () => '$$').replaceAll('"', () => '$\\"');
async function installer(compiler) {
  const files = await walk(packageDirectory);
  const dirs = new Set();
  const lines = [];
  for (const path of files) {
    const rel = relative(packageDirectory, path);
    lines.push('Delete "$INSTDIR\\' + nsisString(rel) + '"');
    for (let dir = dirname(rel); dir !== '.'; dir = dirname(dir)) dirs.add(dir);
  }
  for (const dir of [...dirs].sort((a, b) => b.length - a.length)) {
    lines.push('RMDir "$INSTDIR\\' + nsisString(dir) + '"');
  }
  const uninstallFiles = join(output, 'uninstall-files.nsh');
  await writeFile(uninstallFiles, lines.join('\n') + '\n');
  const setup = join(distribution, 'Lapui-' + manifest.version + '-' + platform + '-setup.exe');
  await run(compiler, [
    '/V2', '/DAPP_VERSION=' + manifest.version, '/DPACKAGE_DIR=' + packageDirectory,
    '/DSETUP_FILE=' + setup, '/DUNINSTALL_FILES=' + uninstallFiles,
    join(root, 'packaging', 'windows', 'installer.nsi'),
  ]);
  log('Installer ready: ' + setup);
}

try {
  if (mode === 'setup') {
    await setupInstaller();
  } else if (mode === 'doctor') {
    await frontendCheck();
    python();
    log('Node ' + process.version + '; frontend ' + frontend + '; Python ' + process.env.PYTHON3);
    await run(...cargo('--version'));
    log('NSIS: ' + (await findNsis() || 'not installed (pnpm build will install it)'));
  } else if (['dev', 'release', 'build'].includes(mode)) {
    if (mode === 'build' && (process.platform !== 'win32' || process.arch !== 'x64')) {
      throw new Error('The current installer definition targets Windows x64.');
    }
    const cargoManifest = await readFile(join(root, 'Cargo.toml'), 'utf8');
    const cargoVersion = cargoManifest.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
    if (cargoVersion !== manifest.version) throw new Error('Keep package.json and Cargo.toml versions in sync.');
    await frontendCheck();
    python();
    if (mode === 'dev') await dev();
    else {
      const compiler = mode === 'build' ? await setupInstaller() : null;
      await release();
      if (compiler && !closing) await installer(compiler);
    }
  } else {
    throw new Error('Use pnpm dev, pnpm release, pnpm build, pnpm doctor, or pnpm setup:installer.');
  }
} catch (error) {
  if (!closing) console.error('[lapui] ' + (error.stack || error));
  process.exitCode = 1;
} finally {
  await cleanup();
}
