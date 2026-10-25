import { createRequire } from 'node:module';
import { resolve, relative, sep, isAbsolute } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const [frontend, output, mode] = process.argv.slice(2);
const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const allowed = resolve(root, 'target', 'desktop');
const outputRelative = relative(allowed, resolve(output));
if (!outputRelative || isAbsolute(outputRelative) || outputRelative.startsWith('..' + sep) || outputRelative === '..') {
  throw new Error('Frontend output must be a subdirectory of target/desktop.');
}
const requireFrontend = createRequire(resolve(frontend, 'package.json'));
const { build } = await import(pathToFileURL(requireFrontend.resolve('vite')).href);
const watching = mode === 'dev';
const result = await build({
  root: frontend,
  base: './',
  build: {
    outDir: output,
    emptyOutDir: true,
    sourcemap: watching,
    reportCompressedSize: !watching,
    minify: watching ? false : 'esbuild',
    watch: watching ? {} : null,
  },
});
if (watching) {
  result.on('event', (event) => {
    if (event.code === 'END') process.send?.({ type: 'built' });
    if (event.code === 'ERROR') {
      console.error(event.error);
      process.send?.({ type: 'build-error' });
    }
  });
  process.on('disconnect', () => {
    void result.close().finally(() => process.exit());
  });
}
