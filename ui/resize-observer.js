// Native-layout observations; no timer, observer thread or mirrored layout tree.
(() => {
  const observers=new Set(), observerState=new WeakMap();
  const entryState=new WeakMap(), sizeState=new WeakMap();
  let count=0;
  const limits=JSON.parse(__lapui_observer_limits);
  const MAX_OBSERVERS=limits.resizeObservers, MAX_TARGETS=limits.resizeTargets;
  class ResizeObserverSize {
    constructor(){throw new TypeError('Illegal constructor');}
    get inlineSize(){return sizeState.get(this)[0];}
    get blockSize(){return sizeState.get(this)[1];}
  }
  const size=(width,height,vertical)=>{
    const value=Object.create(ResizeObserverSize.prototype);
    sizeState.set(value,vertical?[height,width]:[width,height]);
    return Object.freeze([value]);
  };
  class ResizeObserverEntry {
    constructor(){throw new TypeError('Illegal constructor');}
    get target(){return entryState.get(this).target;}
    get contentRect(){return entryState.get(this).rect;}
    get contentBoxSize(){return entryState.get(this).content;}
    get borderBoxSize(){return entryState.get(this).border;}
    get devicePixelContentBoxSize(){return entryState.get(this).device;}
  }
  const entry=(target,sample,scale)=>{
    const value=Object.create(ResizeObserverEntry.prototype);
    entryState.set(value,{
      target,rect:new DOMRectReadOnly(sample[0],sample[1],sample[2],sample[3]),
      content:size(sample[2],sample[3],sample[6]),border:size(sample[4],sample[5],sample[6]),
      device:size(Math.round(sample[2]*scale),Math.round(sample[3]*scale),sample[6])
    });
    return value;
  };
  function validate(target){if(!(target instanceof Element))throw new TypeError('Resize observation requires an Element');}
  class ResizeObserver {
    constructor(callback){
      if(typeof callback!=='function')throw new TypeError('ResizeObserver callback must be a function');
      observerState.set(this,{callback,targets:new Map()});
    }
    observe(target,options={}){
      const state=observerState.get(this);
      if(!state)throw new TypeError('Invalid ResizeObserver receiver');
      validate(target);
      const box=String(options?.box??'content-box');
      if(!['content-box','border-box','device-pixel-content-box'].includes(box))throw new TypeError('Invalid observed box');
      const existing=state.targets.get(target);
      if(!existing && count>=MAX_TARGETS)throw new RangeError('Resize target capacity exceeded');
      if(!state.targets.size && observers.size>=MAX_OBSERVERS)throw new RangeError('Resize observer capacity exceeded');
      if(existing)state.targets.delete(target);else count++;
      state.targets.set(target,{box,last:[0,0]});
      observers.add(this);
      __lapui_render_request();
    }
    unobserve(target){
      const state=observerState.get(this);
      if(!state)throw new TypeError('Invalid ResizeObserver receiver');
      validate(target);
      if(state.targets.delete(target))count--;
      if(!state.targets.size)observers.delete(this);
    }
    disconnect(){
      const state=observerState.get(this);
      if(!state)throw new TypeError('Invalid ResizeObserver receiver');
      count-=state.targets.size;state.targets.clear();observers.delete(this);
    }
  }
  Object.assign(globalThis,{ResizeObserver,ResizeObserverEntry,ResizeObserverSize});
  const same=(a,b)=>a[0]===b[0]&&a[1]===b[1];
  const boxSize=(sample,box,scale)=>{
    const physical=box==='border-box' ? [sample[4],sample[5]] : box==='device-pixel-content-box' ? [Math.round(sample[2]*scale),Math.round(sample[3]*scale)] : [sample[2],sample[3]];
    return sample[6] ? [physical[1],physical[0]] : physical;
  };
  globalThis.__lapui_rendering_update=()=>{
    let changed=__lapui_render_window_notifications();
    changed=__lapui_intersection_update()||changed;
    if(!observers.size)return changed;
    const scale=__lapui_viewport()[2], deadline=performance.now()+limits.deliverySliceMillis;
    let depth=0;
    for(let pass=0;pass<limits.deliveryPasses;pass++){
      const references=new Set();
      for(const observer of observers)for(const target of observerState.get(observer).targets.keys())references.add(target.__ref);
      if(!references.size)return changed;
      const samples=JSON.parse(__lapui_resize_samples([...references]));
      const deliveries=[];
      let skipped=false, nextDepth=Infinity;
      for(const observer of observers){
        const state=observerState.get(observer), active=[];
        for(const [target,record] of state.targets){
          const sample=samples[target.__ref];
          if(!sample)continue;
          const current=boxSize(sample,record.box,scale);
          if(same(record.last,current))continue;
          if(sample[7]<=depth){skipped=true;continue;}
          active.push({target,record,current,value:entry(target,sample,scale)});
          nextDepth=Math.min(nextDepth,sample[7]);
        }
        if(active.length)deliveries.push({observer,state,active});
      }
      if(!deliveries.length){
        if(!skipped)return changed;
        break;
      }
      // Snapshot the whole delivery before any callback changes another target.
      for(const {active} of deliveries)for(const item of active)item.record.last=item.current;
      for(const {observer,state,active} of deliveries){
        const entries=active.filter(item=>state.targets.get(item.target)===item.record).map(item=>item.value);
        if(!entries.length)continue;
        changed=true;
        try{state.callback.call(observer,entries,observer);}
        catch(error){__lapui_report_script_error('resize-observer',String(error?.message||error),String(error?.stack||''),'resize-observer');}
      }
      depth=nextDepth;
      if(performance.now()>=deadline)break;
    }
    const remaining=new Set();
    for(const observer of observers)for(const target of observerState.get(observer).targets.keys())remaining.add(target.__ref);
    const samples=remaining.size ? JSON.parse(__lapui_resize_samples([...remaining])) : {};
    let pending=false;
    for(const observer of observers)for(const [target,record] of observerState.get(observer).targets){
      const sample=samples[target.__ref];
      if(sample&&!same(record.last,boxSize(sample,record.box,scale)))pending=true;
    }
    if(pending){
      __lapui_report_script_error('resize-observer','ResizeObserver loop completed with undelivered notifications.','','resize-observer');
      __lapui_render_request();
    }
    return changed;
  };
})();
