// Bounded MutationObserver subset over the authoritative DOM bridge.
(() => {
  const observers = new Set(), states = new WeakMap(), recordValues = new WeakMap();
  const limits = JSON.parse(__lapui_observer_limits);
  let targetCount = 0, queuedCount = 0, deliveryScheduled = false, overflowReported = false;
  let nativeBatchDepth = 0;
  const pendingStyles = new Map(), pendingTexts = new Map();
  const pageChangeCapacity = 256, pageChangeNodeLimit = 32;
  const pageChanges = [];
  let pageChangeSequence = 0;
  let pageChangeSequenceExhausted = false;

  class MutationRecord {
    constructor() { throw new TypeError('Illegal constructor'); }
    get type() { return recordValues.get(this).type; }
    get target() { return recordValues.get(this).target; }
    get addedNodes() { return recordValues.get(this).addedNodes; }
    get removedNodes() { return recordValues.get(this).removedNodes; }
    get previousSibling() { return recordValues.get(this).previousSibling; }
    get nextSibling() { return recordValues.get(this).nextSibling; }
    get attributeName() { return recordValues.get(this).attributeName; }
    get attributeNamespace() { return null; }
    get oldValue() { return recordValues.get(this).oldValue; }
  }

  function nodeList(nodes = []) {
    const list = nodes.slice();
    Object.defineProperty(list, 'item', {value: index => list[Number(index)] ?? null});
    return Object.freeze(list);
  }

  function record(data) {
    const result = Object.create(MutationRecord.prototype);
    recordValues.set(result, {
      type: data.type, target: data.target, addedNodes: nodeList(data.addedNodes),
      removedNodes: nodeList(data.removedNodes), previousSibling: data.previousSibling ?? null,
      nextSibling: data.nextSibling ?? null, attributeName: data.attributeName ?? null,
      oldValue: data.oldValue ?? null
    });
    return Object.freeze(result);
  }

  function matches(options, data) {
    if (!options[data.type]) return false;
    if (data.type === 'attributes' && options.attributeFilter && !options.attributeFilter.includes(data.attributeName)) return false;
    return true;
  }

  function scheduleDelivery() {
    if (deliveryScheduled) return;
    deliveryScheduled = true;
    queueMicrotask(() => {
      deliveryScheduled = false;
      const deliveries = [];
      for (const observer of observers) {
        const state = states.get(observer);
        if (state.records.length) {
          const records = state.records.splice(0);
          queuedCount -= records.length;
          deliveries.push([observer, state.callback, records]);
        }
      }
      for (const [observer, callback, records] of deliveries) {
        if (!states.has(observer)) continue;
        try { callback.call(observer, records, observer); }
        catch (error) { __lapui_report_script_error('mutation-observer', String(error?.message || error), String(error?.stack || ''), 'mutation-observer'); }
      }
    });
  }

  function recordPageChange(data) {
    if (!data.target) return;
    if (pageChangeSequence < Number.MAX_SAFE_INTEGER) {
      const added = data.addedNodes || [], removed = data.removedNodes || [];
      const record = {
        sequence:++pageChangeSequence,
        type:data.type,
        target:data.target.__ref || '',
        ...(data.attributeName ? {attributeName:data.attributeName} : {}),
        ...(data.propertyName ? {propertyName:data.propertyName} : {}),
        ...(data.controlEvent ? {controlEvent:data.controlEvent} : {}),
        addedCount:added.length,
        added:added.slice(0,pageChangeNodeLimit).map(node=>node.__ref || ''),
        removedCount:removed.length,
        removed:removed.slice(0,pageChangeNodeLimit).map(node=>node.__ref || ''),
        nodesTruncated:added.length>pageChangeNodeLimit || removed.length>pageChangeNodeLimit
      };
      const traceSequence = globalThis.__lapui_notify_page_change?.(record.sequence);
      if (Number.isSafeInteger(traceSequence) && traceSequence > 0) record.debugTraceSequence = traceSequence;
      pageChanges.push(record);
      if (pageChanges.length > pageChangeCapacity) pageChanges.shift();
    } else {
      pageChangeSequenceExhausted = true;
      globalThis.__lapui_notify_page_change?.(0);
    }
  }

  function enqueue(data) {
    if (!data.target) return;
    recordPageChange(data);
    if (queuedCount >= limits.mutationRecords) {
      if (!overflowReported) {
        overflowReported = true;
        __lapui_report_script_error('mutation-observer', 'MutationObserver queue limit exceeded; new records were dropped', '', 'mutation-observer');
      }
      return;
    }
    for (const observer of observers) {
      const state = states.get(observer);
      let current = data.target, selected = [];
      while (current) {
        const options = state.targets.get(current);
        if (options && (current === data.target || options.subtree) && matches(options, data)) selected.push(options);
        current = current.parentNode;
      }
      if (!selected.length) continue;
      const oldValue = data.type === 'attributes'
        ? (selected.some(options => options.attributeOldValue) ? data.oldValue : null)
        : data.type === 'characterData'
          ? (selected.some(options => options.characterDataOldValue) ? data.oldValue : null)
          : null;
      state.records.push(record({...data, oldValue}));
      queuedCount++;
    }
    if (queuedCount) scheduleDelivery();
  }

  function childList(target, addedNodes, removedNodes, previousSibling, nextSibling) {
    enqueue({type:'childList', target, addedNodes, removedNodes, previousSibling, nextSibling});
  }

  function publishTextChange(target, oldValue, oldChildren) {
    if (target.nodeType === 3 || target.nodeType === 8) {
      const current = target.nodeValue;
      if (oldValue !== current) enqueue({type:'characterData', target, oldValue});
      return;
    }
    const newChildren = target.childNodes;
    const added = newChildren.filter(node=>!oldChildren.includes(node));
    const removed = oldChildren.filter(node=>!newChildren.includes(node));
    if (added.length || removed.length) childList(target, added, removed, null, null);
    else if (oldValue !== target.textContent && newChildren.length === 1 && newChildren[0].nodeType === 3) {
      // Lapui may update a sole text node in place for editor/layout stability.
      enqueue({type:'characterData', target:newChildren[0], oldValue});
    }
  }

  class MutationObserver {
    constructor(callback) {
      if (typeof callback !== 'function') throw new TypeError('MutationObserver callback must be a function');
      states.set(this, {callback, targets: new Map(), records: []});
    }
    observe(target, options = {}) {
      const state = states.get(this);
      if (!state) throw new TypeError('Invalid MutationObserver receiver');
      if (!(target instanceof Node)) throw new TypeError('MutationObserver target must be a Node');
      if (options == null || typeof options !== 'object') throw new TypeError('MutationObserver options must be an object');
      const attributesSpecified = Object.hasOwn(options, 'attributes');
      const characterDataSpecified = Object.hasOwn(options, 'characterData');
      const attributes = Boolean(options.attributes || options.attributeOldValue || options.attributeFilter);
      const characterData = Boolean(options.characterData || options.characterDataOldValue);
      if (attributesSpecified && !options.attributes && (options.attributeOldValue || options.attributeFilter)) throw new TypeError('attributeOldValue/attributeFilter require attributes');
      if (characterDataSpecified && !options.characterData && options.characterDataOldValue) throw new TypeError('characterDataOldValue requires characterData');
      const normalized = {
        childList:Boolean(options.childList), attributes, characterData,
        subtree:Boolean(options.subtree), attributeOldValue:Boolean(options.attributeOldValue),
        characterDataOldValue:Boolean(options.characterDataOldValue),
        attributeFilter:options.attributeFilter == null ? null : [...options.attributeFilter].map(name=>String(name).toLowerCase())
      };
      if (!normalized.childList && !normalized.attributes && !normalized.characterData) throw new TypeError('At least one mutation type must be enabled');
      if (!state.targets.has(target) && targetCount >= limits.mutationTargets) throw new RangeError('Mutation target capacity exceeded');
      if (!state.targets.size && observers.size >= limits.mutationObservers) throw new RangeError('Mutation observer capacity exceeded');
      if (!state.targets.has(target)) targetCount++;
      state.targets.set(target, normalized);
      observers.add(this);
    }
    disconnect() {
      const state = states.get(this);
      if (!state) throw new TypeError('Invalid MutationObserver receiver');
      targetCount -= state.targets.size;
      queuedCount -= state.records.length;
      state.targets.clear(); state.records.length = 0; observers.delete(this);
    }
    takeRecords() {
      const state = states.get(this);
      if (!state) throw new TypeError('Invalid MutationObserver receiver');
      const records = state.records.splice(0);
      queuedCount -= records.length;
      return records;
    }
  }

  Object.assign(globalThis, {MutationObserver, MutationRecord});

  const setAttribute = __lapui_set_attribute;
  globalThis.__lapui_set_attribute = (reference, name, value) => {
    const target = __lapui_element(reference), oldValue = __lapui_has_attribute(reference, name) ? __lapui_get_attribute(reference, name) : null;
    setAttribute(reference, name, value);
    const current = __lapui_get_attribute(reference, name);
    if (oldValue !== current) enqueue({type:'attributes', target, attributeName:String(name).toLowerCase(), oldValue});
  };
  const removeAttribute = __lapui_remove_attribute;
  globalThis.__lapui_remove_attribute = (reference, name) => {
    const target = __lapui_element(reference), oldValue = __lapui_has_attribute(reference, name) ? __lapui_get_attribute(reference, name) : null;
    const removed = removeAttribute(reference, name);
    if (removed && oldValue !== null) enqueue({type:'attributes', target, attributeName:String(name).toLowerCase(), oldValue});
    return removed;
  };

  const batchBegin = __lapui_batch_begin;
  globalThis.__lapui_batch_begin = () => { nativeBatchDepth++; return batchBegin(); };
  const batchEnd = __lapui_batch_end;
  globalThis.__lapui_batch_end = () => {
    const result = batchEnd();
    nativeBatchDepth = Math.max(0, nativeBatchDepth - 1);
    if (nativeBatchDepth === 0) {
      for (const [reference, pending] of pendingStyles) {
        const current = __lapui_has_attribute(reference, 'style') ? __lapui_get_attribute(reference, 'style') : null;
        if (pending.oldValue !== current) enqueue({type:'attributes', target:pending.target, attributeName:'style', oldValue:pending.oldValue});
      }
      pendingStyles.clear();
      for (const pending of pendingTexts.values()) publishTextChange(pending.target, pending.oldValue, pending.oldChildren);
      pendingTexts.clear();
    }
    return result;
  };

  for (const [name, native] of [['__lapui_set_style',__lapui_set_style],['__lapui_remove_style',__lapui_remove_style]]) {
    globalThis[name] = (reference, ...args) => {
      const target = __lapui_element(reference), oldValue = __lapui_has_attribute(reference, 'style') ? __lapui_get_attribute(reference, 'style') : null;
      if (nativeBatchDepth > 0 && !pendingStyles.has(reference)) pendingStyles.set(reference, {target, oldValue});
      const result = native(reference, ...args);
      if (nativeBatchDepth > 0) return result;
      const current = __lapui_get_attribute(reference, 'style');
      if (oldValue !== current) enqueue({type:'attributes', target, attributeName:'style', oldValue});
      return result;
    };
  }

  const append = __lapui_append_child;
  globalThis.__lapui_append_child = (parentRef, childRef) => {
    const parent = __lapui_element(parentRef), child = __lapui_element(childRef);
    const previous = child.parentNode;
    if (parent) parent.childNodes;
    if (previous) previous.childNodes;
    if (previous === parent && parent.lastChild === child) return true;
    const oldPrevious = child.previousSibling, oldNext = child.nextSibling;
    if (!append(parentRef, childRef)) return false;
    if (previous) {
      const children = previous.childNodes;
      childList(previous, [], [child], oldPrevious, oldNext);
    }
    parent.childNodes;
    childList(parent, [child], [], child.previousSibling, child.nextSibling);
    return true;
  };

  const insertBefore = __lapui_insert_before;
  globalThis.__lapui_insert_before = (parentRef, childRef, beforeRef) => {
    const parent = __lapui_element(parentRef), child = __lapui_element(childRef), before = beforeRef ? __lapui_element(beforeRef) : null;
    const previous = child.parentNode;
    if (parent) parent.childNodes;
    if (previous) previous.childNodes;
    if (previous === parent && (before === child || child.nextSibling === before)) return true;
    const oldPrevious = child.previousSibling, oldNext = child.nextSibling;
    if (!insertBefore(parentRef, childRef, beforeRef)) return false;
    if (previous) {
      previous.childNodes;
      childList(previous, [], [child], oldPrevious, oldNext);
    }
    parent.childNodes;
    childList(parent, [child], [], child.previousSibling, child.nextSibling);
    return true;
  };

  const removeChild = __lapui_remove_child;
  globalThis.__lapui_remove_child = (parentRef, childRef) => {
    const parent = __lapui_element(parentRef), child = __lapui_element(childRef);
    if (parent) parent.childNodes;
    const previous = child?.previousSibling, next = child?.nextSibling;
    if (!removeChild(parentRef, childRef)) return false;
    parent.childNodes;
    childList(parent, [], [child], previous, next);
    return true;
  };

  const removeNode = __lapui_remove_node;
  globalThis.__lapui_remove_node = reference => {
    const child = __lapui_element(reference), parent = child?.parentNode;
    if (!parent) return removeNode(reference);
    parent.childNodes;
    const previous = child.previousSibling, next = child.nextSibling;
    if (!removeNode(reference)) return false;
    parent.childNodes;
    childList(parent, [], [child], previous, next);
    return true;
  };

  const setText = __lapui_set_text;
  globalThis.__lapui_set_text = (reference, value) => {
    const target = __lapui_element(reference);
    if (!target) return setText(reference, value);
    const oldValue = target.nodeType === 3 || target.nodeType === 8 ? target.nodeValue : target.textContent;
    const oldChildren = target.childNodes;
    if (nativeBatchDepth > 0) {
      if (!pendingTexts.has(reference)) pendingTexts.set(reference, {target, oldValue, oldChildren});
      return setText(reference, value);
    }
    setText(reference, value);
    publishTextChange(target, oldValue, oldChildren);
  };

  const setInnerHtml = __lapui_set_inner_html;
  globalThis.__lapui_set_inner_html = (reference, value) => {
    const target = __lapui_element(reference), oldChildren = target?.childNodes || [];
    const result = setInnerHtml(reference, value);
    if (result && target) {
      const newChildren = target.childNodes;
      const added = newChildren.filter(node=>!oldChildren.includes(node));
      const removed = oldChildren.filter(node=>!newChildren.includes(node));
      if (added.length || removed.length) childList(target, added, removed, null, null);
    }
    return result;
  };

  // A bounded, value-free journal lets external automation detect bridge DOM
  // changes without consuming user MutationObserver capacity. Native Rust
  // mutations remain outside the bridge and are not included.
  globalThis.__lapui_page_changes = (afterSequence, limit) => {
    afterSequence = Number(afterSequence);
    limit = Number(limit);
    if (!Number.isSafeInteger(afterSequence) || afterSequence < 0 ||
        !Number.isInteger(limit) || limit < 1 || limit > 64) {
      throw new TypeError('Invalid page change cursor or limit');
    }
    const oldest = pageChanges[0]?.sequence ?? pageChangeSequence + 1;
    const resyncRequired = afterSequence > pageChangeSequence ||
      (afterSequence < oldest - 1 && pageChanges.length > 0);
    const selected = resyncRequired ? [] : pageChanges
      .filter(record=>record.sequence>afterSequence).slice(0,limit);
    const last = selected.length ? selected[selected.length - 1].sequence : afterSequence;
    return JSON.stringify({
      latestSequence:pageChangeSequence,
      nextSequence:resyncRequired ? pageChangeSequence : last,
      oldestSequence:oldest,
      hasMore:selected.length>0 && last<pageChangeSequence,
      resyncRequired,
      sequenceExhausted:pageChangeSequenceExhausted,
      records:selected
    });
  };
  globalThis.__lapui_record_page_change = (target, propertyName) => {
    if (!target || !['value','checked'].includes(propertyName)) return;
    recordPageChange({type:'property', propertyName, target});
  };
  for (const type of ['input','change']) {
    document.addEventListener(type, event => {
      recordPageChange({type:'control', controlEvent:type, target:event.target});
    });
  }
})();
