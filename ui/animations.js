// Selected two-keyframe style animations used by local component libraries.
(() => {
  const active = new WeakMap();
  const numeric = value => String(value).match(/^(-?\d+(?:\.\d+)?)([a-z%]*)$/i);
  const cssName = name => name.replace(/[A-Z]/g, letter => `-${letter.toLowerCase()}`);
  function easing(value) {
    const presets = {linear:[0,0,1,1],ease:[.25,.1,.25,1],'ease-in':[.42,0,1,1],'ease-out':[0,0,.58,1],'ease-in-out':[.42,0,.58,1]};
    const points = presets[value] || String(value).match(/^cubic-bezier\(([^)]+)\)$/)?.[1].split(',').map(Number);
    if (!points || points.length !== 4 || points.some(number => !Number.isFinite(number)) || points[0] < 0 || points[0] > 1 || points[2] < 0 || points[2] > 1) {
      throw new TypeError('Unsupported animation easing');
    }
    const curve = (t, a, b) => 3*(1-t)*(1-t)*t*a + 3*(1-t)*t*t*b + t*t*t;
    return progress => {
      if (progress <= 0 || progress >= 1) return progress;
      let low = 0, high = 1;
      for (let i = 0; i < 20; i++) {
        const middle = (low + high) / 2;
        if (curve(middle, points[0], points[2]) < progress) low = middle; else high = middle;
      }
      return curve((low + high) / 2, points[1], points[3]);
    };
  }
  class Animation {
    constructor(element, frames, options) {
      if (!(element instanceof Element) || !Array.isArray(frames) || frames.length !== 2) {
        throw new TypeError('Animations require an element and two keyframes');
      }
      options = typeof options === 'number' ? {duration:options} : options;
      this._duration = Number(options?.duration ?? 0);
      if (!Number.isFinite(this._duration) || this._duration < 0) throw new TypeError('Invalid duration');
      if (options?.iterations != null && options.iterations !== 1 || options?.delay) throw new TypeError('Animation repeats and delays are unsupported');
      this._ease = easing(options?.easing ?? 'linear');
      this._fill = options?.fill ?? 'none';
      this._element = element;
      this._properties = Object.keys(frames[1]).filter(name => !['offset','easing','composite'].includes(name)).map(name => {
        const property = cssName(name);
        return {name:property, from:frames[0][name] ?? getComputedStyle(element).getPropertyValue(property), to:frames[1][name], original:element.style.getPropertyValue(property)};
      });
      this.playState = 'running';
      this.onfinish = null;
      this.oncancel = null;
      this.currentTime = 0;
      this.startTime = performance.now();
      this.finished = new Promise((resolve, reject) => { this._resolve = resolve; this._reject = reject; });
      // Cancellation without an installed finished handler is still a valid
      // operation; callers retain the rejected promise to observe it.
      this.finished.catch(() => {});
      const animations = active.get(element) || new Set();
      active.set(element, animations);
      animations.add(this);
      this._apply(0);
      this._request = requestAnimationFrame(timestamp => this._tick(timestamp));
    }
    _apply(progress) {
      const eased = this._ease(progress);
      for (const property of this._properties) {
        const from = numeric(property.from), to = numeric(property.to);
        const value = from && to && from[2] === to[2]
          ? `${Number(from[1]) + (Number(to[1]) - Number(from[1])) * eased}${to[2]}`
          : eased < .5 ? property.from : property.to;
        this._element.style.setProperty(property.name, String(value));
      }
    }
    _restore() {
      for (const property of this._properties) {
        this._element.style.setProperty(property.name, property.original);
      }
      active.get(this._element)?.delete(this);
    }
    _tick(timestamp) {
      if (this.playState !== 'running') return;
      this.currentTime = Math.max(0, timestamp - this.startTime);
      const progress = this._duration === 0 ? 1 : Math.min(1, this.currentTime / this._duration);
      this._apply(progress);
      if (progress === 1) this.finish();
      else this._request = requestAnimationFrame(time => this._tick(time));
    }
    finish() {
      if (this.playState !== 'running') return;
      cancelAnimationFrame(this._request);
      this.currentTime = this._duration;
      this.playState = 'finished';
      if (this._fill === 'forwards' || this._fill === 'both') this._apply(1); else this._restore();
      this._resolve(this);
      this.onfinish?.call(this, new Event('finish'));
    }
    cancel() {
      if (this.playState === 'idle') return;
      const running = this.playState === 'running';
      cancelAnimationFrame(this._request);
      this._restore();
      this.playState = 'idle';
      this.currentTime = null;
      if (running) this._reject(new DOMException('Animation canceled', 'AbortError'));
      this.oncancel?.call(this, new Event('cancel'));
    }
  }
  globalThis.Animation = Animation;
  Element.prototype.animate = function(frames, options) { return new Animation(this, frames, options); };
  Element.prototype.getAnimations = function() { return [...(active.get(this) || [])]; };
})();
