// Viewport-only IntersectionObserver backed by Blitz's resolved client rects.
(() => {
  const observers = new Set(), states = new WeakMap(), entryValues = new WeakMap();
  const limits = JSON.parse(__lapui_observer_limits);
  let targetCount = 0;

  class IntersectionObserverEntry {
    constructor() { throw new TypeError('Illegal constructor'); }
    get time() { return entryValues.get(this).time; }
    get target() { return entryValues.get(this).target; }
    get rootBounds() { return entryValues.get(this).rootBounds; }
    get boundingClientRect() { return entryValues.get(this).boundingClientRect; }
    get intersectionRect() { return entryValues.get(this).intersectionRect; }
    get isIntersecting() { return entryValues.get(this).isIntersecting; }
    get intersectionRatio() { return entryValues.get(this).intersectionRatio; }
  }

  function parseMargin(value) {
    const tokens = String(value).trim().split(/\s+/);
    if (!tokens[0] || tokens.length > 4) throw new SyntaxError('Invalid rootMargin');
    const parsed = tokens.map(token => {
      const match = /^(-?(?:\d+\.?\d*|\.\d+))(px|%)$/.exec(token);
      if (!match) throw new SyntaxError('rootMargin accepts px or % values');
      const amount = Number(match[1]);
      if (!Number.isFinite(amount) || Math.abs(amount) > limits.intersectionMarginPx) throw new RangeError('rootMargin exceeds the supported limit');
      return [amount, match[2]];
    });
    if (parsed.length === 1) return [parsed[0], parsed[0], parsed[0], parsed[0]];
    if (parsed.length === 2) return [parsed[0], parsed[1], parsed[0], parsed[1]];
    if (parsed.length === 3) return [parsed[0], parsed[1], parsed[2], parsed[1]];
    return parsed;
  }

  function thresholds(value) {
    const values = (Array.isArray(value) ? value : [value ?? 0]).map(Number);
    if (!values.length) values.push(0);
    if (!values.length || values.length > limits.intersectionThresholds || values.some(item => !Number.isFinite(item) || item < 0 || item > 1)) {
      throw new RangeError('threshold must contain 1..' + limits.intersectionThresholds + ' values in [0, 1]');
    }
    return [...new Set(values)].sort((a, b) => a - b);
  }

  function expandRoot(viewport, margin) {
    const [width, height] = viewport;
    const resolve = item => item[1] === '%' ? item[0] * width / 100 : item[0];
    const [top, right, bottom, left] = margin.map(resolve);
    const x = -left, y = -top;
    return new DOMRectReadOnly(x, y,
      Math.max(0, width + left + right),
      Math.max(0, height + top + bottom));
  }

  function rectIntersection(target, root) {
    const left = Math.max(target.left, root.left), top = Math.max(target.top, root.top);
    const right = Math.min(target.right, root.right), bottom = Math.min(target.bottom, root.bottom);
    const intersects = right >= left && bottom >= top;
    return {
      intersects,
      rect: intersects ? new DOMRectReadOnly(left, top, Math.max(0, right-left), Math.max(0, bottom-top)) : new DOMRectReadOnly()
    };
  }

  function makeEntry(target, bounding, root, intersection, intersects, ratio) {
    const entry = Object.create(IntersectionObserverEntry.prototype);
    entryValues.set(entry, {time: performance.now(), target, rootBounds: root,
      boundingClientRect: bounding, intersectionRect: intersection,
      isIntersecting: intersects, intersectionRatio: ratio});
    return Object.freeze(entry);
  }

  class IntersectionObserver {
    constructor(callback, options = {}) {
      if (typeof callback !== 'function') throw new TypeError('IntersectionObserver callback must be a function');
      if (options == null || typeof options !== 'object') throw new TypeError('IntersectionObserver options must be an object');
      if (options.root != null) throw new DOMException('Element roots are not supported', 'NotSupportedError');
      states.set(this, {callback, targets: new Map(), margin: parseMargin(options.rootMargin ?? '0px'), thresholds: thresholds(options.threshold)});
    }
    get root() { return null; }
    get rootMargin() {
      const state = states.get(this);
      if (!state) throw new TypeError('Invalid IntersectionObserver receiver');
      return state.margin.map(item => item[0] + item[1]).join(' ');
    }
    get thresholds() {
      const state = states.get(this);
      if (!state) throw new TypeError('Invalid IntersectionObserver receiver');
      return [...state.thresholds];
    }
    observe(target) {
      const state = states.get(this);
      if (!state) throw new TypeError('Invalid IntersectionObserver receiver');
      if (!(target instanceof Element)) throw new TypeError('Intersection observation requires an Element');
      if (state.targets.has(target)) return;
      if (targetCount >= limits.intersectionTargets) throw new RangeError('Intersection target capacity exceeded');
      if (!state.targets.size && observers.size >= limits.intersectionObservers) throw new RangeError('Intersection observer capacity exceeded');
      targetCount++;
      state.targets.set(target, {delivered: false, ratio: 0, intersecting: false});
      observers.add(this);
      __lapui_render_request();
    }
    unobserve(target) {
      const state = states.get(this);
      if (!state) throw new TypeError('Invalid IntersectionObserver receiver');
      if (state.targets.delete(target)) targetCount--;
      if (!state.targets.size) observers.delete(this);
    }
    disconnect() {
      const state = states.get(this);
      if (!state) throw new TypeError('Invalid IntersectionObserver receiver');
      targetCount -= state.targets.size;
      state.targets.clear();
      observers.delete(this);
    }
    takeRecords() { if (!states.has(this)) throw new TypeError('Invalid IntersectionObserver receiver'); return []; }
  }

  Object.assign(globalThis, {IntersectionObserver, IntersectionObserverEntry});
  globalThis.__lapui_intersection_update = () => {
    if (!observers.size) return false;
    const refs = new Set();
    for (const observer of observers) for (const target of states.get(observer).targets.keys()) refs.add(target.__ref);
    if (!refs.size || refs.size > 2048) return false;
    const samples = JSON.parse(__lapui_intersection_samples([...refs]));
    const viewport = __lapui_viewport();
    let delivered = false;
    for (const observer of observers) {
      const state = states.get(observer), entries = [];
      const root = expandRoot(viewport, state.margin);
      for (const [target, previous] of state.targets) {
        const sample = samples[target.__ref] || [0, 0, 0, 0, 0];
        const hasBox = sample[0] === 1;
        const bounding = new DOMRectReadOnly(sample[1], sample[2], sample[3], sample[4]);
        const result = hasBox ? rectIntersection(bounding, root) : {intersects: false, rect: new DOMRectReadOnly()};
        const area = bounding.width * bounding.height;
        const ratio = result.intersects ? (area > 0 ? result.rect.width * result.rect.height / area : 1) : 0;
        const crossed = previous.delivered && state.thresholds.some(value =>
          (previous.ratio < value && ratio >= value) || (previous.ratio > value && ratio <= value));
        if (!previous.delivered || crossed || previous.intersecting !== result.intersects) {
          entries.push(makeEntry(target, bounding, root, result.rect, result.intersects, ratio));
        }
        previous.delivered = true;
        previous.ratio = ratio;
        previous.intersecting = result.intersects;
      }
      if (entries.length) {
        delivered = true;
        try { state.callback.call(observer, entries, observer); }
        catch (error) { __lapui_report_script_error('intersection-observer', String(error?.message || error), String(error?.stack || ''), 'intersection-observer'); }
      }
    }
    return delivered;
  };
})();
