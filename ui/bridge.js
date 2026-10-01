(() => {
  if (typeof globalThis.DOMException !== 'function') {
    globalThis.DOMException = class DOMException extends Error {
      constructor(message, name = 'Error') { super(message); this.name = name; }
    };
  }
  const elements = new Map();
  const rectValues = new WeakMap();
  const selectionDirections = new WeakMap();
  class DOMRectReadOnly {
    constructor(x = 0, y = 0, width = 0, height = 0) { rectValues.set(this, [Number(x), Number(y), Number(width), Number(height)]); }
    get x() { return rectValues.get(this)[0]; }
    get y() { return rectValues.get(this)[1]; }
    get width() { return rectValues.get(this)[2]; }
    get height() { return rectValues.get(this)[3]; }
    get top() { return Math.min(this.y, this.y + this.height); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    get left() { return Math.min(this.x, this.x + this.width); }
    toJSON() { return { x:this.x, y:this.y, width:this.width, height:this.height, top:this.top, right:this.right, bottom:this.bottom, left:this.left }; }
    static fromRect(rect = {}) { return new this(rect.x, rect.y, rect.width, rect.height); }
  }
  class DOMRect extends DOMRectReadOnly {}
  for (const [index, name] of ['x','y','width','height'].entries()) {
    Object.defineProperty(DOMRect.prototype, name, {
      get() { return rectValues.get(this)[index]; },
      set(value) { rectValues.get(this)[index] = Number(value); }, configurable:true
    });
  }
  globalThis.DOMRect = DOMRect;
  globalThis.DOMRectReadOnly = DOMRectReadOnly;
  const styleViews = new WeakSet();
  class CSSStyleDeclaration {
    constructor() { throw new TypeError('Illegal constructor'); }
    static [Symbol.hasInstance](value) { return styleViews.has(value); }
  }
  globalThis.CSSStyleDeclaration = CSSStyleDeclaration;
  globalThis.CSSStyleProperties = CSSStyleDeclaration;
  const documentFragments = new Set();
  // The native DOM is authoritative. These edges describe ownership only:
  // a connected document keeps its wrappers/listeners alive, while retaining
  // any node in a detached tree keeps its parents and descendants alive.
  const nodeState = new WeakMap();
  const windowReference = `window:${__lapui_document_ref()}`;
  const scrollWatches = new Map();
  const observationLimits=JSON.parse(__lapui_observer_limits);
  let lastViewport = __lapui_viewport();
  const styleOwner = Symbol('style owner');
  const retiredNodes = new FinalizationRegistry(reference => {
    if (elements.get(reference)?.deref()) return;
    elements.delete(reference);
    documentFragments.delete(reference);
    const root = __lapui_detached_root(reference);
    if (root && !elements.get(root)?.deref()) __lapui_collect_detached(root);
  });
  const pendingOwnership = new Set();
  let ownershipBatchDepth = 0;
  const pending = new Map();
  const timers = new Map();
  let nextTimerId = 1;
  const scheduleTimer = (callback, delay, repeat, args) => {
    if (typeof callback !== 'function') throw new TypeError('Timer callback must be a function');
    if (timers.size >= 1024 || nextTimerId > 2147483647) throw new RangeError('Timer capacity exceeded');
    delay = Number(delay);
    delay = Number.isFinite(delay) ? Math.min(2147483647, Math.max(1, Math.trunc(delay))) : 1;
    const id = nextTimerId++;
    const timer = { callback, delay, repeat, args };
    if (!__lapui_timer_arm(id, delay)) throw new Error('Timer scheduler is unavailable');
    timers.set(id, timer);
    return id;
  };
  globalThis.setTimeout = (callback, delay = 0, ...args) => scheduleTimer(callback, delay, false, args);
  globalThis.setInterval = (callback, delay = 0, ...args) => scheduleTimer(callback, delay, true, args);
  globalThis.clearTimeout = globalThis.clearInterval = id => {
    id = Number(id);
    if (!Number.isInteger(id) || !timers.has(id)) return;
    timers.delete(id);
    __lapui_timer_clear(id);
  };
  globalThis.queueMicrotask = callback => {
    if (typeof callback !== 'function') throw new TypeError('Microtask callback must be a function');
    Promise.resolve().then(callback);
  };
  globalThis.__lapui_fire_timer = id => {
    const timer = timers.get(id);
    if (!timer) return;
    if (!timer.repeat) timers.delete(id);
    try { timer.callback.apply(globalThis, timer.args); }
    catch (error) { __lapui_report_script_error(`timer:${id}`, String(error?.message || error), String(error?.stack || ''), 'timer'); }
    if (timer.repeat && timers.get(id) === timer && !__lapui_timer_arm(id, timer.delay)) timers.delete(id);
  };
  const eventSources = new Map();
  globalThis.performance = { now: () => __lapui_now(), timeOrigin: __lapui_time_origin };
  const animationCallbacks = new Map();
  let nextAnimationId = 1;
  globalThis.requestAnimationFrame = callback => {
    if (typeof callback !== 'function') throw new TypeError('Animation callback must be a function');
    if (animationCallbacks.size >= 1024 || nextAnimationId > 2147483647) throw new RangeError('Animation callback capacity exceeded');
    const id = nextAnimationId++;
    if (!__lapui_animation_request(id)) throw new Error('Animation scheduler is unavailable');
    animationCallbacks.set(id, callback);
    return id;
  };
  globalThis.cancelAnimationFrame = id => {
    id = Number(id);
    if (!Number.isInteger(id) || !animationCallbacks.has(id)) return;
    animationCallbacks.delete(id);
    __lapui_animation_cancel(id);
  };
  globalThis.__lapui_fire_animation_frame = (id, timestamp) => {
    const callback = animationCallbacks.get(id);
    if (!callback) return;
    animationCallbacks.delete(id);
    try { callback.call(globalThis, timestamp); }
    catch (error) { __lapui_report_script_error(`animation-frame:${id}`, String(error?.message || error), String(error?.stack || ''), 'animation-frame'); }
  };
  let nextRequestId = 1;
  function actionCatalog(request) {
    return new Promise((resolve,reject)=>{
      const encoded=JSON.stringify(request);
      if(nextRequestId>2147483647)throw new RangeError('Request ID capacity exceeded');
      const requestId=nextRequestId++;
      pending.set(requestId,{resolve,reject});
      try {
        const failure=__lapui_action_catalog(requestId,encoded);
        if(failure){pending.delete(requestId);const error=JSON.parse(failure);reject(Object.assign(new Error(error.message),{code:error.code}));}
      }catch(error){pending.delete(requestId);throw error;}
    });
  }
  let nextStreamId = 1;
  let focusTransitionDuringKeydown = null;
  let keydownDispatchDepth = 0;
  const focusClearedDuringKeydown = 'lapui-focus-cleared';
  const dispatchFocusTransition = (previous, current) => {
    if (previous && previous !== current) {
      const path = __lapui_event_path(previous);
      __lapui_dispatch('blur', previous, path);
      __lapui_dispatch('focusout', previous, path);
    }
    if (current && current !== previous) {
      const path = __lapui_event_path(current);
      __lapui_dispatch('focus', current, path);
      __lapui_dispatch('focusin', current, path);
    }
  };

  class Element {
    static [Symbol.hasInstance](value) {
      return Boolean(value && typeof value.__ref === 'string' && __lapui_node_type(value.__ref) === 1 && !documentFragments.has(value.__ref));
    }
    constructor(reference, tagName = '') {
      this.__ref = reference;
      this.tagName = (tagName || __lapui_tag(reference)).toUpperCase();
      const cssName = name => name.startsWith('--') ? name : name.replace(/[A-Z]/g, char => `-${char.toLowerCase()}`);
      const cssValue = name => __lapui_get_style(reference, String(name)).replace(/\s*!important\s*$/i, '').trim();
      const style = {
        setProperty(name, value, priority = '') {
          const normalizedPriority = String(priority).toLowerCase();
          if (normalizedPriority && normalizedPriority !== 'important') return;
          const property = String(name);
          const text = String(value);
          if (!text) __lapui_remove_style(reference, property);
          else if (normalizedPriority === 'important') {
            const current = __lapui_get_css_text(reference);
            __lapui_set_attribute(reference, 'style', `${current}${current ? ' ' : ''}${property}: ${text} !important;`);
          }
          else __lapui_set_style(reference, property, text + (normalizedPriority ? ' !important' : ''));
        },
        getPropertyValue(name) { return cssValue(name); },
        removeProperty(name) {
          const previous = cssValue(name);
          __lapui_remove_style(reference, String(name));
          return previous;
        },
        get cssText() { return __lapui_get_css_text(reference); },
        set cssText(value) { __lapui_set_attribute(reference, 'style', String(value)); }
      };
      Object.defineProperty(style, styleOwner, { value: this });
      this.style = new Proxy(style, {
        get(target, property, receiver) {
          if (Reflect.has(target, property)) return Reflect.get(target, property, receiver);
          if (typeof property === 'string') return cssValue(cssName(property));
        },
        set(target, property, value) {
          if (property === 'cssText') { __lapui_set_attribute(reference, 'style', String(value)); return true; }
          if (typeof property === 'string') {
            const name = cssName(property);
            const text = String(value);
            if (!text) __lapui_remove_style(reference, name);
            else __lapui_set_style(reference, name, text);
            return true;
          }
          return false;
        },
        deleteProperty(target, property) {
          if (typeof property !== 'string') return false;
          __lapui_remove_style(reference, cssName(property));
          return true;
        }
      });
      styleViews.add(this.style);
      const element = this;
      const tokens = () => (element.className.match(/\S+/g) || []);
      const validateToken = token => {
        if (!token) throw new DOMException('The token must not be empty', 'SyntaxError');
        if (/\s/.test(token)) throw new DOMException('The token must not contain whitespace', 'InvalidCharacterError');
        return token;
      };
      this.classList = {
        get length() { return tokens().length; },
        get value() { return element.className; },
        set value(value) { element.className = String(value); },
        contains(token) { return tokens().includes(validateToken(String(token))); },
        add(...items) {
          const current = tokens();
          for (const item of items) {
            const token = validateToken(String(item));
            if (!current.includes(token)) current.push(token);
          }
          element.className = current.join(' ');
        },
        remove(...items) {
          const remove = new Set(items.map(item => validateToken(String(item))));
          element.className = tokens().filter(token => !remove.has(token)).join(' ');
        },
        toggle(item, force) {
          const token = validateToken(String(item));
          const current = tokens();
          const present = current.includes(token);
          const shouldAdd = arguments.length > 1 ? Boolean(force) : !present;
          if (shouldAdd && !present) current.push(token);
          if (!shouldAdd && present) current.splice(current.indexOf(token), 1);
          element.className = current.join(' ');
          return shouldAdd;
        },
        replace(oldItem, newItem) {
          const oldToken = validateToken(String(oldItem));
          const newToken = validateToken(String(newItem));
          const current = tokens();
          const index = current.indexOf(oldToken);
          if (index < 0) return false;
          if (current.includes(newToken)) current.splice(index, 1);
          else current[index] = newToken;
          element.className = current.join(' ');
          return true;
        },
        item(index) { return tokens()[Number(index)] ?? null; },
        [Symbol.iterator]() { return tokens()[Symbol.iterator](); }
      };
    }
    get nodeType() { return documentFragments.has(this.__ref) ? 11 : __lapui_node_type(this.__ref); }
    get ownerDocument() { return this.nodeType === 9 ? null : document; }
    get namespaceURI() { return this.nodeType === 1 ? 'http://www.w3.org/1999/xhtml' : null; }
    get type() { return this.getAttribute('type') || (this.tagName === 'INPUT' ? 'text' : this.tagName === 'BUTTON' ? 'submit' : ''); }
    set type(value) { this.setAttribute('type', value); }
    get nodeName() {
      if (this.nodeType === 3) return '#text';
      if (this.nodeType === 8) return '#comment';
      if (this.nodeType === 9) return '#document';
      if (this.nodeType === 11) return '#document-fragment';
      return this.tagName;
    }
    get nodeValue() { return this.nodeType === 3 || this.nodeType === 8 ? __lapui_get_text(this.__ref) : null; }
    set nodeValue(value) {
      if (this.nodeType === 3 || this.nodeType === 8) {
        __lapui_set_text(this.__ref, value == null ? '' : String(value));
        globalThis.__lapui_form_tree?.(this);
      }
    }
    get parentNode() {
      const reference = __lapui_parent(this.__ref);
      return reference ? getElement(reference) : null;
    }
    get parentElement() {
      const parent = this.parentNode;
      return parent?.nodeType === 1 ? parent : null;
    }
    get nextSibling() {
      const reference = __lapui_sibling(this.__ref, 1);
      return reference ? getElement(reference) : null;
    }
    get previousSibling() {
      const reference = __lapui_sibling(this.__ref, -1);
      return reference ? getElement(reference) : null;
    }
    get childNodes() { return JSON.parse(__lapui_children(this.__ref)).map(getElement); }
    get firstChild() { return this.childNodes[0] ?? null; }
    get lastChild() { return this.childNodes.slice(-1)[0] ?? null; }
    get children() { return this.childNodes.filter(child => child.nodeType === 1); }
    get childElementCount() { return this.children.length; }
    contains(other) {
      for (let node = other; node; node = node.parentNode) if (node === this) return true;
      return false;
    }
    get id() { return __lapui_get_attribute(this.__ref, 'id'); }
    set id(value) { this.setAttribute('id', value); }
    get className() { return __lapui_get_attribute(this.__ref, 'class'); }
    set className(value) { this.setAttribute('class', value); }
    get disabled() { return this.hasAttribute('disabled'); }
    set disabled(value) {
      if (Boolean(value)) this.setAttribute('disabled', '');
      else this.removeAttribute('disabled');
    }
    get checked() { return __lapui_get_checked(this.__ref); }
    set checked(value) { __lapui_set_checked(this.__ref, Boolean(value)); }
    get selectionStart() { return this._textSelection()[0]; }
    set selectionStart(value) { this.setSelectionRange(value, this.selectionEnd); }
    get selectionEnd() { return this._textSelection()[1]; }
    set selectionEnd(value) { this.setSelectionRange(this.selectionStart, value); }
    get selectionDirection() { return this._textSelection()[2]; }
    _textSelection() {
      const type = this.type.toLowerCase();
      if (this.tagName !== 'TEXTAREA'
          && !(this.tagName === 'INPUT' && ['text','search','tel','url','password'].includes(type))) {
        throw new DOMException('Text selection is unavailable for this control type', 'InvalidStateError');
      }
      const selection = JSON.parse(__lapui_get_selection(this.__ref)) || [0, 0, 'none'];
      const requested = selectionDirections.get(this);
      if (requested && requested.start === selection[0] && requested.end === selection[1]) {
        selection[2] = requested.direction;
      } else {
        selectionDirections.delete(this);
      }
      return selection;
    }
    setSelectionRange(start, end, direction = 'none') {
      this._textSelection();
      direction = String(direction);
      if (!['none','forward','backward'].includes(direction)) throw new TypeError('Invalid selection direction');
      const length = this.value.length;
      const offset = value => {
        const number = Number(value);
        return Number.isFinite(number) ? Math.max(0, Math.min(length, Math.trunc(number))) : 0;
      };
      start = offset(start);
      end = Math.max(start, offset(end));
      if (!__lapui_set_selection(this.__ref, start, end, direction)) {
        throw new DOMException('The control cannot be selected', 'InvalidStateError');
      }
      selectionDirections.set(this, { start, end, direction });
    }
    get isConnected() { return __lapui_is_connected(this.__ref); }
    getBoundingClientRect() { return new DOMRect(...__lapui_bounding_rect(this.__ref)); }
    get offsetLeft() { return __lapui_offset_metrics(this.__ref)[0]; }
    get offsetTop() { return __lapui_offset_metrics(this.__ref)[1]; }
    get offsetWidth() { return __lapui_offset_metrics(this.__ref)[2]; }
    get offsetHeight() { return __lapui_offset_metrics(this.__ref)[3]; }
    get offsetParent() { return getElement(__lapui_offset_parent(this.__ref)); }
    getClientRects() {
      const list = __lapui_client_rects(this.__ref).map(rect => new DOMRect(...rect));
      Object.defineProperty(list, 'item', {value: index => list[Number(index)] ?? null});
      return list;
    }
    get scrollLeft() { return __lapui_layout_metrics(this.__ref)[0]; }
    set scrollLeft(value) { this.scrollTo({left: value}); }
    get scrollTop() { return __lapui_layout_metrics(this.__ref)[1]; }
    set scrollTop(value) { this.scrollTo({top: value}); }
    get clientWidth() { return __lapui_layout_metrics(this.__ref)[2]; }
    get clientHeight() { return __lapui_layout_metrics(this.__ref)[3]; }
    get clientLeft() { return __lapui_layout_metrics(this.__ref)[4]; }
    get clientTop() { return __lapui_layout_metrics(this.__ref)[5]; }
    get scrollWidth() { return __lapui_layout_metrics(this.__ref)[6]; }
    get scrollHeight() { return __lapui_layout_metrics(this.__ref)[7]; }
    scrollTo(...args) { scrollElement(this, false, args); }
    scroll(...args) { this.scrollTo(...args); }
    scrollBy(...args) { scrollElement(this, true, args); }

    click() { lapui.activate(this.__ref); }
    get textContent() { return __lapui_get_text(this.__ref); }
    set textContent(value) {
      __lapui_set_text(this.__ref, String(value));
      if (ownershipBatchDepth) pendingOwnership.add(this);
      else syncChildren(this);
    }
    get innerHTML() { return __lapui_get_inner_html(this.__ref); }
    set innerHTML(value) {
      if (!__lapui_set_inner_html(this.__ref, String(value))) throw new Error('innerHTML update failed');
      syncChildren(this);
      globalThis.__lapui_form_tree?.(this);
    }
    get value() { return __lapui_get_value(this.__ref); }
    set value(value) { __lapui_set_value(this.__ref, String(value)); }
    focus() {
      const previous = document.activeElement;
      if (!__lapui_set_focus(this.__ref)) return;
      const previousRef = previous && previous !== document.body ? previous.__ref : '';
      if (keydownDispatchDepth > 0) focusTransitionDuringKeydown = this.__ref;
      dispatchFocusTransition(previousRef, this.__ref);
    }
    blur() {
      if (__lapui_clear_focus(this.__ref)) {
        if (keydownDispatchDepth > 0) focusTransitionDuringKeydown = focusClearedDuringKeydown;
        dispatchFocusTransition(this.__ref, '');
      }
    }
    cloneNode(deep = false) {
      const reference = __lapui_clone(this.__ref, Boolean(deep));
      if (!reference) throw new Error('cloneNode failed');
      if (this.nodeType === 11) documentFragments.add(reference);
      return getElement(reference);
    }
    appendChild(child) {
      if (child?.nodeType === 11) {
        for (const node of child.childNodes.slice()) this.appendChild(node);
        return child;
      }
      const previous = child.parentNode;
      if (!__lapui_append_child(this.__ref, child.__ref)) throw new Error('appendChild failed');
      if (previous && previous !== this) syncChildren(previous);
      syncChildren(this);
      globalThis.__lapui_form_tree?.(child);
      return child;
    }
    append(...items) { for (const item of items) this.appendChild(asNode(item)); }
    prepend(...items) {
      const reference = this.firstChild;
      for (const item of items) this.insertBefore(asNode(item), reference);
    }
    replaceChildren(...items) {
      for (const child of this.childNodes) this.removeChild(child);
      this.append(...items);
    }
    insertBefore(child, referenceNode = null) {
      if (child?.nodeType === 11) {
        for (const node of child.childNodes.slice()) this.insertBefore(node, referenceNode);
        return child;
      }
      const reference = referenceNode == null ? '' : referenceNode.__ref;
      const previous = child.parentNode;
      if (!__lapui_insert_before(this.__ref, child.__ref, reference)) throw new Error('insertBefore failed');
      if (previous && previous !== this) syncChildren(previous);
      syncChildren(this);
      globalThis.__lapui_form_tree?.(child);
      return child;
    }
    removeChild(child) {
      if (!__lapui_remove_child(this.__ref, child.__ref)) throw new Error('removeChild failed');
      syncChildren(this);
      if(this.tagName==='TEXTAREA')globalThis.__lapui_form_tree?.(this);
      return child;
    }
    remove() {
      const previous = this.parentNode;
      __lapui_remove_node(this.__ref);
      if (previous) {
        syncChildren(previous);
        if(previous.tagName==='TEXTAREA')globalThis.__lapui_form_tree?.(previous);
      }
    }
    setAttribute(name, value) { __lapui_set_attribute(this.__ref, String(name).toLowerCase(), String(value)); }
    getAttribute(name) {
      name = String(name).toLowerCase();
      return __lapui_has_attribute(this.__ref, name) ? __lapui_get_attribute(this.__ref, name) : null;
    }
    hasAttribute(name) { return __lapui_has_attribute(this.__ref, String(name).toLowerCase()); }
    removeAttribute(name) { __lapui_remove_attribute(this.__ref, String(name).toLowerCase()); }
    querySelector(selector) {
      const reference = __lapui_query(String(selector), this.__ref);
      return reference ? getElement(reference) : null;
    }
    querySelectorAll(selector) {
      return JSON.parse(__lapui_query_all(String(selector), this.__ref)).map(getElement);
    }
    addEventListener(type, callback, options = false) {
      if (callback == null) return;
      if (typeof callback !== 'function' && typeof callback.handleEvent !== 'function') throw new TypeError('Invalid event listener');
      type = String(type);
      const capture = typeof options === 'boolean' ? options : Boolean(options?.capture);
      const listeners = nodeState.get(this).listeners;
      const key = type;
      const callbacks = listeners.get(key) || [];
      if (callbacks.some(record => record.callback === callback && record.capture === capture)) return;
      callbacks.push({ callback, capture, once: Boolean(options?.once), passive: Boolean(options?.passive) });
      listeners.set(key, callbacks);
      if (key === 'scroll' && this !== globalThis && this !== globalThis.document && !scrollWatches.has(this.__ref)) {
        if (scrollWatches.size >= observationLimits.scrollTargets) for (const [ref,watch] of scrollWatches) if (!watch.target.deref()) scrollWatches.delete(ref);
        if (scrollWatches.size >= observationLimits.scrollTargets) {
          callbacks.pop(); if (!callbacks.length) listeners.delete(key);
          throw new RangeError('Scroll observation capacity exceeded');
        }
        const metrics = __lapui_layout_metrics(this.__ref);
        scrollWatches.set(this.__ref,{target:new WeakRef(this),last:[metrics[0],metrics[1]]});
      }
    }
    removeEventListener(type, callback, options = false) {
      const capture = typeof options === 'boolean' ? options : Boolean(options?.capture);
      const listeners = nodeState.get(this).listeners;
      const key = String(type);
      const callbacks = listeners.get(key) || [];
      const index = callbacks.findIndex(record => record.callback === callback && record.capture === capture);
      if (index >= 0) callbacks.splice(index, 1);
      if (callbacks.length) listeners.set(key, callbacks);
      else {
        listeners.delete(key);
        if (key === 'scroll') scrollWatches.delete(this.__ref);
      }
    }
  }

  const abortState = new WeakMap();
  class LapuiAbortSignal {
    constructor(key) {
      if (key !== abortState) throw new TypeError('Use AbortController to create a signal');
      abortState.set(this, { aborted: false, reason: undefined, listeners: [] });
      this.onabort = null;
    }
    get aborted() { return abortState.get(this).aborted; }
    get reason() { return abortState.get(this).reason; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    addEventListener(type, callback, options = {}) {
      if (String(type) !== 'abort' || callback == null) return;
      if (typeof callback !== 'function' && typeof callback.handleEvent !== 'function') throw new TypeError('Invalid listener');
      const capture = typeof options === 'boolean' ? options : Boolean(options?.capture);
      const listeners = abortState.get(this).listeners;
      if (!listeners.some(item => item.callback === callback && item.capture === capture)) listeners.push({ callback, capture, once: Boolean(options?.once) });
    }
    removeEventListener(type, callback, options = {}) {
      if (String(type) !== 'abort') return;
      const capture = typeof options === 'boolean' ? options : Boolean(options?.capture);
      const state = abortState.get(this);
      state.listeners = state.listeners.filter(item => item.callback !== callback || item.capture !== capture);
    }
    static abort(reason) { const controller = new LapuiAbortController(); controller.abort(reason); return controller.signal; }
    static timeout(delay) {
      if (!Number.isInteger(delay) || delay < 0 || delay > 2147483647) throw new RangeError('Invalid timeout');
      const controller = new LapuiAbortController();
      setTimeout(() => controller.abort(new DOMException('The operation timed out', 'TimeoutError')), delay);
      return controller.signal;
    }
  }
  class LapuiAbortController {
    constructor() { Object.defineProperty(this, 'signal', { value: new LapuiAbortSignal(abortState), enumerable: true }); }
    abort(reason = new DOMException('The operation was aborted', 'AbortError')) {
      const signal = this.signal;
      const state = abortState.get(signal);
      if (state.aborted) return;
      state.aborted = true;
      state.reason = reason;
      const event = { type: 'abort', target: signal, currentTarget: signal, bubbles: false, cancelable: false };
      const invoke = callback => {
        try {
          if (typeof callback === 'function') callback.call(signal, event);
          else callback.handleEvent(event);
        } catch (error) { __lapui_report_script_error('abort-listener', String(error?.message || error), String(error?.stack || '')); }
      };
      for (const item of [...state.listeners]) {
        if (!state.listeners.includes(item)) continue;
        if (item.once) signal.removeEventListener('abort', item.callback, item.capture);
        invoke(item.callback);
      }
      if (typeof signal.onabort === 'function') invoke(signal.onabort);
    }
  }
  globalThis.AbortController = LapuiAbortController;
  globalThis.AbortSignal = LapuiAbortSignal;

  class LapuiResponse {
    constructor(raw) {
      this.status = raw.status;
      this.ok = raw.ok;
      this.url = raw.url;
      this.headers = new LapuiHeaders(raw.headers);
      this._body = raw.body;
      this._bodyUsed = false;
    }
    get bodyUsed() { return this._bodyUsed; }
    _consume() {
      if (this.bodyUsed) throw new TypeError('Response body has already been consumed');
      this._bodyUsed = true;
      const body = this._body;
      this._body = null;
      return body;
    }
    async text() { return this._consume(); }
    async json() { return JSON.parse(this._consume()); }
    clone() {
      if (this.bodyUsed) throw new TypeError('Response body has already been consumed');
      return new LapuiResponse({status: this.status, ok: this.ok, url: this.url, headers: this.headers._values, body: this._body});
    }
  }

  class LapuiHeaders {
    constructor(values) {
      this._values = Object.create(null);
      if (values == null) return;
      const entries = typeof values[Symbol.iterator] === 'function' ? values : Object.entries(values);
      for (const entry of entries) {
        if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError('Headers require name/value pairs');
        this.append(entry[0], entry[1]);
      }
    }
    _name(name) {
      name = String(name).toLowerCase();
      if (!/^[-!#$%&'*+.^_`|~0-9a-z]+$/.test(name)) throw new TypeError('Invalid header name');
      return name;
    }
    _value(value) {
      value = String(value).replace(/^[\t ]+|[\t ]+$/g, '');
      if (/[\0\r\n]/.test(value)) throw new TypeError('Invalid header value');
      return value;
    }
    get(name) { return this._values[this._name(name)] ?? null; }
    has(name) { return Object.hasOwn(this._values, this._name(name)); }
    set(name, value) { this._values[this._name(name)] = this._value(value); }
    append(name, value) {
      name = this._name(name); value = this._value(value);
      this._values[name] = Object.hasOwn(this._values, name) ? this._values[name] + ', ' + value : value;
    }
    delete(name) { delete this._values[this._name(name)]; }
    *entries() { for (const name of Object.keys(this._values).sort()) yield [name, this._values[name]]; }
    *keys() { for (const [name] of this) yield name; }
    *values() { for (const [, value] of this) yield value; }
    [Symbol.iterator]() { return this.entries(); }
    forEach(callback, thisArg) { for (const [name, value] of this) callback.call(thisArg, value, name, this); }
  }
  globalThis.Headers = LapuiHeaders;

  class LapuiEventSource {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSED = 2;
    constructor(url) {
      this.url = String(url);
      this.readyState = LapuiEventSource.CONNECTING;
      this.onopen = null;
      this.onmessage = null;
      this.onerror = null;
      this._listeners = new Map();
      this._id = nextStreamId++;
      if (this._id > 0x7fffffff) throw new Error('Stream identities exhausted');
      throwStreamError(__lapui_event_source_open(this._id, this.url));
      eventSources.set(this._id, this);
    }
    addEventListener(type, callback) {
      const callbacks = this._listeners.get(type) || [];
      callbacks.push(callback);
      this._listeners.set(type, callbacks);
    }
    removeEventListener(type, callback) {
      const callbacks = this._listeners.get(type) || [];
      this._listeners.set(type, callbacks.filter(item => item !== callback));
    }
    close() {
      if (this.readyState === LapuiEventSource.CLOSED) return;
      this.readyState = LapuiEventSource.CLOSED;
      eventSources.delete(this._id);
      __lapui_event_source_close(this._id);
    }
    _receive(raw) {
      if (raw.kind === 'open') this.readyState = LapuiEventSource.OPEN;
      if (raw.kind === 'error') {
        this.readyState = raw.fatal ? LapuiEventSource.CLOSED : LapuiEventSource.CONNECTING;
        if (raw.fatal) eventSources.delete(this._id);
      }
      const event = { type: raw.type, data: raw.data, lastEventId: raw.lastEventId, code: raw.code, message: raw.message, fatal: raw.fatal, target: this };
      const handler = raw.type === 'open' ? this.onopen : raw.type === 'error' ? this.onerror : raw.type === 'message' ? this.onmessage : null;
      if (handler) handler(event);
      for (const callback of [...(this._listeners.get(raw.type) || [])]) callback(event);
    }
  }

  class LapuiWebSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;
    constructor(url) {
      this.url = String(url);
      this.readyState = LapuiWebSocket.CONNECTING;
      this.onopen = null;
      this.onmessage = null;
      this.onerror = null;
      this.onclose = null;
      this._listeners = new Map();
      this._id = nextStreamId++;
      if (this._id > 0x7fffffff) throw new Error('Stream identities exhausted');
      throwStreamError(__lapui_websocket_open(this._id, this.url));
      eventSources.set(this._id, this);
    }
    addEventListener(type, callback) {
      const callbacks = this._listeners.get(type) || [];
      callbacks.push(callback);
      this._listeners.set(type, callbacks);
    }
    removeEventListener(type, callback) {
      const callbacks = this._listeners.get(type) || [];
      this._listeners.set(type, callbacks.filter(item => item !== callback));
    }
    send(data) {
      if (this.readyState !== LapuiWebSocket.OPEN) throw new Error('WebSocket is not open');
      let error;
      if (typeof data === 'string') error = __lapui_websocket_send_text(this._id, data);
      else if (data instanceof ArrayBuffer) error = __lapui_websocket_send_binary(this._id, data, 0, data.byteLength);
      else if (ArrayBuffer.isView(data)) error = __lapui_websocket_send_binary(this._id, data.buffer, data.byteOffset, data.byteLength);
      else throw new TypeError('WebSocket.send accepts strings and binary buffers');
      throwStreamError(error);
    }
    get bufferedAmount() { return __lapui_websocket_buffered(this._id); }
    close() {
      if (this.readyState >= LapuiWebSocket.CLOSING) return;
      this.readyState = LapuiWebSocket.CLOSING;
      __lapui_websocket_close(this._id);
    }
    _receive(raw) {
      if (raw.type === 'open') this.readyState = LapuiWebSocket.OPEN;
      if (raw.type === 'close') {
        this.readyState = LapuiWebSocket.CLOSED;
        eventSources.delete(this._id);
      }
      const data = raw.dataType === 'binary' && typeof Uint8Array !== 'undefined' ? new Uint8Array(raw.data) : raw.data;
      const event = { type: raw.type, data, code: raw.code, message: raw.message, reason: raw.reason, target: this };
      const handler = raw.type === 'open' ? this.onopen : raw.type === 'message' ? this.onmessage : raw.type === 'error' ? this.onerror : raw.type === 'close' ? this.onclose : null;
      if (handler) handler(event);
      for (const callback of [...(this._listeners.get(raw.type) || [])]) callback(event);
    }
  }

  function throwStreamError(encoded) {
    if (!encoded) return;
    const error = JSON.parse(encoded);
    throw Object.assign(new Error(error.message), { code: error.code });
  }

  const getOrCreate = canonical => {
    if (canonical === windowReference) return globalThis;
    if (globalThis.document && canonical === globalThis.document.__ref) return globalThis.document;
    let element = elements.get(canonical)?.deref();
    if (!element) {
      element = new Element(canonical);
      nodeState.set(element, { parent: null, children: [], listeners: new Map(), initialized: false });
      elements.set(canonical, new WeakRef(element));
      retiredNodes.register(element, canonical);
      __lapui_dom_retired();
    }
    return element;
  };
  const syncOne = node => {
    const state = nodeState.get(node);
    const children = JSON.parse(__lapui_children(node.__ref)).map(getOrCreate);
    const current = new Set(children);
    let retired = false;
    for (const child of state.children) {
      if (!current.has(child) && nodeState.get(child).parent === node) {
        nodeState.get(child).parent = null;
        retired = true;
      }
    }
    if (retired) __lapui_dom_retired();
    state.children = children;
    state.initialized = true;
    for (const child of children) nodeState.get(child).parent = node;
    return children.filter(child => !nodeState.get(child).initialized);
  };
  const syncChildren = node => {
    const work = [node];
    while (work.length) {
      for (const child of syncOne(work.pop())) work.push(child);
    }
  };
  const getElement = reference => {
    if (reference === windowReference) return globalThis;
    const canonical = __lapui_resolve(String(reference));
    if (!canonical) return null;
    const element = getOrCreate(canonical);
    if (!nodeState.get(element).initialized) syncChildren(element);
    return element;
  };
  globalThis.__lapui_element = getElement;
  const asNode = value => value && typeof value.__ref === 'string' && Number.isInteger(value.nodeType)
    ? value
    : getElement(__lapui_create_text(String(value)));

  // Vue and other renderers use these browser constructors for mount-time
  // type checks. The host currently exposes HTML nodes only, so the SVG and
  // MathML constructors serve as compatibility sentinels until namespace
  // aware element creation is implemented.
  globalThis.Node = class Node {
    constructor() { throw new TypeError('Illegal constructor'); }
    static [Symbol.hasInstance](value) { return value !== globalThis && nodeState.has(value); }
  };
  for (const [name,value] of Object.entries({ELEMENT_NODE:1,ATTRIBUTE_NODE:2,TEXT_NODE:3,COMMENT_NODE:8,DOCUMENT_NODE:9,DOCUMENT_FRAGMENT_NODE:11})) {
    Object.defineProperty(Node,name,{value});
  }
  globalThis.Element = Element;
  globalThis.HTMLElement = Element;
  globalThis.HTMLIFrameElement = class HTMLIFrameElement {
    static [Symbol.hasInstance](value) { return value instanceof Element && value.tagName === 'IFRAME'; }
  };
  globalThis.window = globalThis;
  globalThis.self = globalThis;
  globalThis.SVGElement = class SVGElement {};
  globalThis.MathMLElement = class MathMLElement {};
  globalThis.DocumentFragment = class DocumentFragment {
    static [Symbol.hasInstance](value) {
      return Boolean(value && documentFragments.has(value.__ref));
    }
  };

  globalThis.getComputedStyle = (element, pseudo = null) => {
    if (!(element instanceof Element) || element.nodeType !== 1) throw new TypeError('getComputedStyle requires an Element');
    if (pseudo !== null && String(pseudo) !== '') throw new DOMException('Pseudo-element style queries are not supported','NotSupportedError');
    const names = () => __lapui_computed_names(element.__ref);
    const value = name => __lapui_computed_value(element.__ref,String(name));
    const readonly = () => { throw new DOMException('Computed styles are read-only','NoModificationAllowedError'); };
    const style = {
      getPropertyValue(name) { return value(name); },
      getPropertyPriority() { return ''; },
      setProperty: readonly, removeProperty: readonly,
      get length() { return names().length; },
      item(index) { return names()[Number(index) >>> 0] ?? ''; },
      get cssText() { return ''; },
      get parentRule() { return null; },
      [Symbol.iterator]() { return names()[Symbol.iterator](); }
    };
    Object.defineProperty(style,styleOwner,{value:element});
    Object.setPrototypeOf(style,CSSStyleDeclaration.prototype);
    const proxy = new Proxy(style,{
      get(target,property,receiver) {
        if (Reflect.has(target,property)) return Reflect.get(target,property,receiver);
        if (typeof property !== 'string') return undefined;
        if (/^(0|[1-9][0-9]*)$/.test(property)) return names()[Number(property)];
        const name = property === 'cssFloat' ? 'float' : property.startsWith('--') ? property : property.replace(/[A-Z]/g,char=>'-'+char.toLowerCase());
        return value(name);
      },
      set: readonly, deleteProperty: readonly, defineProperty: readonly,
      ownKeys(target) { return [...names().map((_,index)=>String(index)),...Reflect.ownKeys(target)]; },
      getOwnPropertyDescriptor(target,property) {
        if (typeof property==='string' && /^(0|[1-9][0-9]*)$/.test(property)) {
          const name = names()[Number(property)];
          return name === undefined ? undefined : {value:name,writable:false,enumerable:true,configurable:true};
        }
        return Reflect.getOwnPropertyDescriptor(target,property);
      }
    });
    styleViews.add(proxy);
    return proxy;
  };

  const pendingScrollEvents = new Set();
  function scrollElement(element, relative, args) {
    const options = args[0] !== null && typeof args[0] === 'object' ? args[0] : {left: args[0], top: args[1]};
    const behavior = options.behavior === undefined ? 'auto' : String(options.behavior);
    if (!['auto','instant','smooth'].includes(behavior)) throw new TypeError('Invalid scroll behavior');
    if (behavior === 'smooth') throw new DOMException('Smooth JS scrolling is not supported', 'NotSupportedError');
    const coordinate = value => { if (value === undefined) return undefined; const number = Number(value); return Number.isFinite(number) ? number : 0; };
    if (!__lapui_scroll(element.__ref,coordinate(options.left),coordinate(options.top),relative)) return;
    const target = element === document.documentElement ? document : element;
    if (pendingScrollEvents.has(target)) return;
    pendingScrollEvents.add(target);
    queueMicrotask(() => {
      pendingScrollEvents.delete(target);
      if (target === document || target.isConnected) {
        const metrics = __lapui_layout_metrics(element.__ref);
        if (target === document) { lastViewport[3]=metrics[0]; lastViewport[4]=metrics[1]; }
        else {
          const watch=scrollWatches.get(target.__ref);
          if(watch)watch.last=[metrics[0],metrics[1]];
        }
        __lapui_dispatch('scroll',target.__ref,__lapui_event_path(target.__ref));
      }
    });
  }

  globalThis.document = {
    __ref: __lapui_document_ref(),
    nodeType: 9,
    nodeName: '#document',
    ownerDocument: null,
    defaultView: globalThis,
    oninput: null,
    addEventListener(...args) { Element.prototype.addEventListener.apply(this, args); },
    removeEventListener(...args) { Element.prototype.removeEventListener.apply(this, args); },
    get documentElement() { return this.querySelector('html'); },
    get scrollingElement() { return this.documentElement; },
    get body() { return getElement(__lapui_body()); },
    get activeElement() {
      const active = getElement(__lapui_get_active_element());
      if (!active || !active.isConnected) return this.body;
      const tag = active.tagName.toLowerCase();
      const focusable = ['button', 'input', 'select', 'textarea'].includes(tag)
        || (tag === 'a' && active.hasAttribute('href'))
        || active.hasAttribute('tabindex');
      return focusable && !active.disabled ? active : this.body;
    },
    createElement(tagName) {
      const tag = String(tagName).toLowerCase();
      const reference = __lapui_create_element(tag);
      if (!reference) throw new Error(`unsupported element name: ${tag}`);
      return getElement(reference);
    },
    createTextNode(value) {
      return getElement(__lapui_create_text(String(value)));
    },
    createComment(value) {
      return getElement(__lapui_create_comment(String(value)));
    },
    createDocumentFragment() {
      const reference = __lapui_create_element('div');
      if (!reference) throw new Error('DocumentFragment creation failed');
      documentFragments.add(reference);
      return getElement(reference);
    },
    querySelector(selector) {
      const reference = __lapui_query(String(selector), '');
      return reference ? getElement(reference) : null;
    },
    querySelectorAll(selector) {
      return JSON.parse(__lapui_query_all(String(selector), '')).map(getElement);
    },
    getElementById(id) {
      id = String(id);
      if (!__lapui_exists(id)) return null;
      return getElement(id);
    }
  };

  for (const [name,index] of [['scrollX',0],['pageXOffset',0],['scrollY',1],['pageYOffset',1],['innerWidth',2],['innerHeight',3]]) {
    Object.defineProperty(globalThis,name,{get:()=>__lapui_layout_metrics(document.documentElement.__ref)[index],configurable:true});
  }
  globalThis.scrollTo = (...args) => document.documentElement.scrollTo(...args);
  globalThis.scroll = globalThis.scrollTo;
  globalThis.scrollBy = (...args) => document.documentElement.scrollBy(...args);

  nodeState.set(globalThis,{listeners:new Map()});
  globalThis.addEventListener = (...args) => Element.prototype.addEventListener.apply(globalThis,args);
  globalThis.removeEventListener = (...args) => Element.prototype.removeEventListener.apply(globalThis,args);
  // Actual viewport and native scroll changes are checked at rendering time.
  // Weak target references do not retain otherwise unreachable detached nodes.
  globalThis.__lapui_render_window_notifications = () => {
    let changed=false;
    const viewport=__lapui_viewport(), previous=lastViewport;
    lastViewport=viewport;
    if(viewport[0]!==previous[0] || viewport[1]!==previous[1]) {
      __lapui_dispatch('resize',windowReference);
      changed=true;
    }
    if(viewport[3]!==previous[3] || viewport[4]!==previous[4]) {
      __lapui_dispatch('scroll',document.__ref,__lapui_event_path(document.__ref));
      changed=true;
    }
    const targets=[];
    for(const [ref,watch] of scrollWatches) {
      const target=watch.target.deref();
      if(!target)scrollWatches.delete(ref);
      else if(target.isConnected)targets.push([ref,target,watch]);
    }
    if(targets.length) {
      const samples=JSON.parse(__lapui_resize_samples(targets.map(([ref])=>ref)));
      for(const [ref,target,watch] of targets) {
        const sample=samples[ref];
        if(!sample || scrollWatches.get(ref)!==watch || !target.isConnected)continue;
        const next=[sample[8],sample[9]];
        if(next[0]!==watch.last[0] || next[1]!==watch.last[1]) {
          watch.last=next;
          __lapui_dispatch('scroll',ref,__lapui_event_path(ref));
          changed=true;
        }
      }
    }
    return changed;
  };

  nodeState.set(document, { parent: null, children: [], listeners: new Map(), initialized: false });
  syncChildren(document);

  globalThis.lapui = {
    batch(callback) {
      __lapui_batch_begin();
      ownershipBatchDepth++;
      try { return callback(); }
      finally {
        __lapui_batch_end();
        if (--ownershipBatchDepth === 0) {
          const dirty = [...pendingOwnership];
          pendingOwnership.clear();
          for (const node of dirty) syncChildren(node);
        }
      }
    },
    controls() { return JSON.parse(__lapui_controls()); },
    diagnostics() { return JSON.parse(__lapui_script_diagnostics()); },
    networkStatus() { return JSON.parse(__lapui_network_status()); },
    observe() { return JSON.parse(__lapui_host_observe()); },
    actions: {
      list(options = {}) {
        if (!options || typeof options !== 'object' || Array.isArray(options) ||
            Object.keys(options).some(key=>!['prefix','scope','cursor','limit'].includes(key))) {
          return Promise.reject(Object.assign(new Error('Unsupported action-list options'),{code:'invalid_request'}));
        }
        return actionCatalog({...options, method:'actions.list'});
      },
      describe(action) { return actionCatalog({method:'actions.describe', action:String(action)}); },
      check(action, args = {}) { return actionCatalog({method:'actions.check', action:String(action), args}); }
    },
    changes: {
      subscribe(options = {}) {
        return new Promise((resolve, reject) => {
          const encoded = JSON.stringify(options);
          if (nextRequestId > 2147483647) throw new RangeError('Request ID capacity exceeded');
          const requestId = nextRequestId++;
          pending.set(requestId, {resolve, reject});
          try {
            const failure = __lapui_changes_subscribe(requestId, encoded);
            if (failure) {
              pending.delete(requestId);
              const error = JSON.parse(failure);
              reject(Object.assign(new Error(error.message), {code: error.code}));
            }
          } catch (error) { pending.delete(requestId); throw error; }
        });
      }
    },
    operation(id) {
      const response = JSON.parse(__lapui_operation(String(id), false));
      if (!response.ok) throw Object.assign(new Error(response.error.message), {code:response.error.code});
      return response.snapshot;
    },
    cancelOperation(id) {
      const response = JSON.parse(__lapui_operation(String(id), true));
      if (!response.ok) throw Object.assign(new Error(response.error.message), {code:response.error.code});
      return response.snapshot;
    },
    waitOperation(id, afterRevision) {
      return new Promise((resolve, reject) => {
        if (!Number.isSafeInteger(afterRevision) || afterRevision < 0) throw new TypeError('afterRevision must be a non-negative safe integer');
        const requestId = nextRequestId++;
        pending.set(requestId, {resolve, reject});
        try { __lapui_operation_wait(String(id), requestId, String(afterRevision)); }
        catch (error) { pending.delete(requestId); throw error; }
      });
    },
    trace(afterSequence = 0) {
      if (!Number.isSafeInteger(afterSequence) || afterSequence < 0) throw new TypeError('afterSequence must be a non-negative safe integer');
      return JSON.parse(__lapui_host_trace(String(afterSequence)));
    },
    focus(reference) {
      const target = getElement(String(reference));
      if (!target) return false;
      target.focus();
      return document.activeElement === target;
    },
    activate(reference) {
      reference = String(reference);
      if (!__lapui_exists(reference) || !__lapui_is_enabled(reference)) return false;
      __lapui_dispatch('click', reference, __lapui_event_path(reference));
      return true;
    },
    fill(reference, value) {
      reference = String(reference);
      const target = getElement(reference);
      if (!target || target.hasAttribute('readonly') || target.getAttribute('aria-readonly') === 'true') return false;
      if (target.tagName !== 'SELECT' && target.tagName !== 'TEXTAREA' && !(target.tagName === 'INPUT' && ['text', 'password', 'email', 'number', 'search', 'tel', 'url'].includes((target.getAttribute('type') || 'text').toLowerCase()))) return false;
      if (!__lapui_is_enabled(reference)) return false;
      if (target.tagName === 'SELECT') {
        if (target.multiple) return false;
        target.value = String(value);
      }
      else if (!__lapui_set_value(reference, String(value))) return false;
      const path = __lapui_event_path(reference);
      __lapui_dispatch('input', reference, path);
      __lapui_dispatch('change', reference, path);
      return true;
    },
    check(reference, checked = true) {
      reference = String(reference);
      if (typeof checked !== 'boolean') return false;
      if (!__lapui_exists(reference) || !__lapui_is_enabled(reference)) return false;
      if (!__lapui_set_checked(reference, checked)) return false;
      const path = __lapui_event_path(reference);
      __lapui_dispatch('input', reference, path);
      __lapui_dispatch('change', reference, path);
      return true;
    },
    fetch(url, init = {}) {
      return new Promise((resolve, reject) => {
        const signal = init.signal;
        if (signal != null && !(signal instanceof LapuiAbortSignal)) throw new TypeError('Invalid AbortSignal');
        signal?.throwIfAborted();
        const encoded = JSON.stringify({
          url: String(url), method: String(init.method || 'GET'), headers: Object.fromEntries(new LapuiHeaders(init.headers)),
          body: init.body == null ? undefined : String(init.body)
        });
        signal?.throwIfAborted();
        if (nextRequestId > 2147483647) throw new RangeError('Request ID capacity exceeded');
        const requestId = nextRequestId++;
        const cleanup = () => signal?.removeEventListener('abort', onAbort);
        const onAbort = () => {
          pending.delete(requestId);
          cleanup();
          __lapui_fetch_abort(requestId);
          reject(signal.reason);
        };
        pending.set(requestId, { resolve, reject, cleanup });
        signal?.addEventListener('abort', onAbort, { once: true });
        try {
          const code = __lapui_fetch(requestId, encoded);
          if (code) throw Object.assign(new Error(code === 'network_busy' ? 'At most 16 fetch requests may be outstanding' : 'Fetch request could not be started'), { code });
        } catch (error) { pending.delete(requestId); cleanup(); reject(error); }
      });
    },
    invoke(name, args = {}, options = {}) {
      return new Promise((resolve, reject) => {
        if (options && Object.hasOwn(options, 'expectedVersion') && !Number.isSafeInteger(options.expectedVersion)) throw new TypeError('expectedVersion must be a safe integer');
        const encodedArgs = JSON.stringify(args);
        const encodedOptions = JSON.stringify(options);
        if (encodedArgs === undefined || encodedOptions === undefined) throw new TypeError('invocation requires JSON arguments and options');
        const requestId = nextRequestId++;
        pending.set(requestId, { resolve, reject });
        try { __lapui_host_invoke(String(name), requestId, encodedArgs, encodedOptions); }
        catch (error) { pending.delete(requestId); throw error; }
      });
    }
  };

  globalThis.fetch = (input, init) => lapui.fetch(input, init).then(raw => new LapuiResponse(raw));
  globalThis.EventSource = LapuiEventSource;
  globalThis.WebSocket = LapuiWebSocket;
  globalThis.__lapui_track_module = (source, promise) => {
    promise.catch(error => __lapui_report_script_error(
      String(source), String(error?.message || error), String(error?.stack || '')
    ));
  };

  let armedSpace = null;
  const dispatch = (type, targetId, path = '', detail = '{}', fields = {}) => {
    if (type === 'keydown' && keydownDispatchDepth === 0) focusTransitionDuringKeydown = null;
    const target = getElement(targetId);
    if ((type === 'blur' || type === 'focusout') && armedSpace?.deref() === target) armedSpace = null;
    const ids = path ? path.split('\n') : [targetId];
    if (ids.at(-1) === document.__ref) ids.push(windowReference);
    if (type === 'input') globalThis.__lapui_form_input?.(target);
    const click = type === 'click';
    const checkable = click && target?.tagName === 'INPUT' && ['checkbox', 'radio'].includes(target.type.toLowerCase());
    let label = null;
    if (click) {
      if (['INPUT', 'BUTTON', 'SELECT', 'TEXTAREA'].includes(target?.tagName) && !__lapui_is_enabled(targetId)) {
        return JSON.stringify({dispatched: false, defaultPrevented: false, nativeDefaultHandled: true});
      }
      for (const id of ids) {
        const element = getElement(id);
        if (['INPUT', 'BUTTON', 'SELECT', 'TEXTAREA'].includes(element?.tagName)) {
          if (!__lapui_is_enabled(id)) return JSON.stringify({dispatched: false, defaultPrevented: false, nativeDefaultHandled: true});
          break;
        }
        if (element?.tagName === 'A' && element.hasAttribute('href')) break;
        if (element?.tagName === 'LABEL') { label = element; break; }
      }
    }
    const before = checkable ? __lapui_radio_group(targetId).split('\n').filter(Boolean).map(id => [id, getElement(id).checked]) : [];
    const targetBefore = checkable && target.checked;
    if (checkable) target.checked = target.type.toLowerCase() === 'radio' ? true : !targetBefore;
    let dispatched = false;
    const bubbles = type === 'scroll' ? target === document : !['focus', 'blur', 'mouseenter', 'mouseleave', 'pointerenter', 'pointerleave', 'resize', 'invalid'].includes(type);
    const cancelable = ['keydown', 'keyup', 'keypress', 'click', 'dblclick', 'submit', 'reset', 'invalid'].includes(type);
    const event = {
      ...JSON.parse(detail),
      ...fields,
      type,
      target,
      currentTarget: null,
      bubbles,
      cancelable,
      defaultPrevented: false,
      eventPhase: 0,
      timeStamp: performance.now(),
      preventDefault() { if (this.cancelable && !this.__passive) this.defaultPrevented = true; },
      stopPropagation() { this.__stopped = true; },
      stopImmediatePropagation() { this.__stopped = true; this.__immediate = true; },
      composedPath() { return ids.map(getElement); },
      __stopped: false
    };
    const invoke = (id, capture, phase) => {
      event.currentTarget = getElement(id);
      event.eventPhase = phase;
      const listeners = nodeState.get(event.currentTarget).listeners;
      const key = type;
      for (const record of [...(listeners.get(key) || [])]) {
        if (record.capture !== capture || !(listeners.get(key) || []).includes(record)) continue;
        if (record.once) event.currentTarget.removeEventListener(type, record.callback, capture);
        event.__passive = record.passive;
        dispatched = true;
        try {
          if (typeof record.callback === 'function') record.callback.call(event.currentTarget, event);
          else record.callback.handleEvent(event);
        } catch (error) {
          __lapui_report_script_error(`event:${type}:${id}`, String(error?.message || error), String(error?.stack || ''), 'event');
        } finally { event.__passive = false; }
        if (event.__immediate) break;
      }
      if (!capture && !event.__immediate) {
        const handler = event.currentTarget[`on${type}`];
        if (typeof handler === 'function') {
          dispatched = true;
          try { if (handler.call(event.currentTarget, event) === false) event.preventDefault(); }
          catch (error) { __lapui_report_script_error(`event:${type}:${id}`, String(error?.message || error), String(error?.stack || ''), 'event'); }
        }
      }
    };
    if (type === 'keydown') keydownDispatchDepth++;
    try {
      for (let index = ids.length - 1; index > 0 && !event.__stopped; index--) invoke(ids[index], true, 1);
      if (!event.__stopped) {
        invoke(ids[0], true, 2);
        if (!event.__immediate) invoke(ids[0], false, 2);
        if (bubbles) for (let index = 1; index < ids.length && !event.__stopped; index++) invoke(ids[index], false, 3);
      }
    } finally {
      if (type === 'keydown') keydownDispatchDepth--;
      event.currentTarget = null;
      event.eventPhase = 0;
    }
    if (checkable) {
      if (event.defaultPrevented) {
        if (target.type.toLowerCase() === 'radio') {
          const previouslyChecked = before.find(([, checked]) => checked);
          const group = __lapui_radio_group(targetId).split('\n');
          if (previouslyChecked && group.includes(previouslyChecked[0])) __lapui_set_checked(previouslyChecked[0], true);
          else __lapui_restore_checked(targetId, false);
        } else __lapui_restore_checked(targetId, targetBefore);
      } else if (target.isConnected && targetBefore !== target.checked) {
        const currentPath = __lapui_event_path(targetId);
        __lapui_dispatch('input', targetId, currentPath);
        __lapui_dispatch('change', targetId, currentPath);
      }
    }
    if (label && !event.defaultPrevented) {
      const control = getElement(__lapui_label_control(label.__ref));
      if (control && __lapui_is_enabled(control.__ref)) {
        __lapui_dispatch('click', control.__ref, __lapui_event_path(control.__ref));
        control.focus();
      }
    }
    let formDefault=false;
    if(click && globalThis.__lapui_form_click) {
      try{formDefault=__lapui_form_click(target,ids.map(getElement),event);}
      catch(error){formDefault=true;__lapui_report_script_error('form-default',String(error?.message||error),String(error?.stack||''),'form');}
    }
    let keyboardDefault = false;
    const button = target?.tagName === 'BUTTON' || (target?.tagName === 'INPUT' && ['button', 'submit', 'reset'].includes(target.type.toLowerCase()));
    const checkInput = target?.tagName === 'INPUT' && ['checkbox', 'radio'].includes(target.type.toLowerCase());
    if (type === 'keydown' && !event.ctrlKey && !event.altKey && !event.metaKey && __lapui_is_enabled(targetId)) {
      if (event.key === ' ' && (button || checkInput)) {
        keyboardDefault = true;
        if (!event.defaultPrevented && !event.repeat) armedSpace = new WeakRef(target);
        else if (event.defaultPrevented) armedSpace = null;
      } else if (event.key === 'Enter' && button) {
        keyboardDefault = true;
        if (!event.defaultPrevented && !event.repeat) lapui.activate(targetId);
      } else if (target?.tagName === 'INPUT' && target.type.toLowerCase() === 'radio' && ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) {
        keyboardDefault = true;
        if (!event.defaultPrevented) {
          const group = __lapui_radio_group(targetId).split('\n').map(getElement).filter(element => element?.isConnected && __lapui_is_enabled(element.__ref));
          const current = group.indexOf(target);
          if (current >= 0 && group.length) {
            const backwards = ['ArrowLeft', 'ArrowUp'].includes(event.key);
            const next = group[(current + (backwards ? -1 : 1) + group.length) % group.length];
            next.focus(); lapui.activate(next.__ref);
          }
        }
      } else if (target?.tagName === 'SELECT' && ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) {
        keyboardDefault = true;
        if (!event.defaultPrevented) {
          const options = target.options.filter(option => !option.disabled && !option.parentElement?.disabled);
          const current = options.findIndex(option => option.selected);
          const backwards = ['ArrowLeft', 'ArrowUp'].includes(event.key);
          const base = current >= 0 ? current : backwards ? options.length : -1;
          const next = event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1
            : Math.max(0, Math.min(options.length - 1, base + (backwards ? -1 : 1)));
          if (options[next]) {
            options[next].selected = true;
            __lapui_dispatch('input', targetId, __lapui_event_path(targetId));
            __lapui_dispatch('change', targetId, __lapui_event_path(targetId));
          }
        }
      }
    }
    if(type==='keydown' && event.key==='Enter' && !button && !event.ctrlKey && !event.altKey && !event.metaKey && globalThis.__lapui_form_enter) {
      try { if(__lapui_form_enter(target,event))keyboardDefault=true; }
      catch(error){keyboardDefault=true;__lapui_report_script_error('form-default',String(error?.message||error),String(error?.stack||''),'form');}
    }
    if (type === 'keyup' && event.key === ' ') {
      const armed = armedSpace?.deref(); armedSpace = null;
      if (armed === target && !event.defaultPrevented && !event.ctrlKey && !event.altKey && !event.metaKey && document.activeElement === target && __lapui_is_enabled(targetId)) {
        keyboardDefault = true; lapui.activate(targetId);
      }
    }
    return JSON.stringify({ dispatched, defaultPrevented: event.defaultPrevented, nativeDefaultHandled: Boolean(checkable || label || keyboardDefault || formDefault) });
  };
  const clicking = new Set();
  globalThis.__lapui_dispatch = (type, targetId, ...args) => {
    if (type !== 'click') return dispatch(type, targetId, ...args);
    const target = getElement(targetId);
    if (clicking.has(target)) return JSON.stringify({dispatched: false, defaultPrevented: false, nativeDefaultHandled: true});
    clicking.add(target);
    try { return dispatch(type, targetId, ...args); }
    finally { clicking.delete(target); }
  };
  globalThis.__lapui_take_focus_transition = () => {
    const transition = focusTransitionDuringKeydown;
    focusTransitionDuringKeydown = null;
    return transition === null ? '' : transition;
  };
  globalThis.__lapui_dispatch_focus_transition = (previous, current) => {
    dispatchFocusTransition(String(previous), String(current));
  };

  globalThis.__lapui_stream_event = (id, data) => {
    const stream = eventSources.get(id);
    if (stream) stream._receive(data);
  };

  globalThis.__lapui_complete = (requestId, ok, payload) => {
    const data = JSON.parse(payload);
    if (requestId === 0) {
      if (ok && typeof globalThis.__lapui_render === 'function') globalThis.__lapui_render(data);
      return;
    }
    const callback = pending.get(requestId);
    if (!callback) return;
    pending.delete(requestId);
    callback.cleanup?.();
    if (ok) callback.resolve(data);
    else callback.reject(Object.assign(new Error(data.message), { code: data.code }));
  };
})();

// Developer-only, opt-in metadata trace. No input values or result bodies.
lapui.debugTrace=Object.freeze({
  configure({enabled,clear=false}={}){
    if(typeof enabled!=='boolean'||typeof clear!=='boolean')throw new TypeError('Trace flags must be boolean');
    __lapui_debug_trace_configure(enabled,clear);
    return this.read();
  },
  read({afterSequence=0,limit=64}={}){
    if(!Number.isSafeInteger(afterSequence)||afterSequence<0||!Number.isInteger(limit)||limit<1||limit>128)throw new RangeError('Invalid trace cursor or limit');
    const reply=JSON.parse(__lapui_debug_trace_read(String(afterSequence),limit));
    if(!reply.ok)throw new RangeError(reply.error.message);
    return reply.observation;
  }
});
