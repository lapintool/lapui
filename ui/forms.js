// Local form semantics over native DOM/editor state; no navigation transport.
(() => {
  const limits=JSON.parse(__lapui_form_limits);
  const states=new WeakMap(), collections=new WeakMap(), validityViews=new WeakMap();
  const busySubmit=new WeakSet(), busyReset=new WeakSet(), dataState=new WeakMap();
  const optionStates=new WeakMap();
  const proto=Element.prototype, descriptor=name=>Object.getOwnPropertyDescriptor(proto,name);
  const oldValue=descriptor('value'), oldChecked=descriptor('checked'), oldText=descriptor('textContent'), oldHTML=descriptor('innerHTML');
  const oldSet=proto.setAttribute, oldRemove=proto.removeAttribute, oldClone=proto.cloneNode;
  const node=reference=>reference ? __lapui_element(reference) : null;
  const textTypes=new Set(['text','search','tel','url','email','password','number']);
  const knownTypes=new Set([...textTypes,'hidden','checkbox','radio','button','submit','reset','image','file','date','month','week','time','datetime-local','range','color']);
  const associated=target=>target instanceof Element && ['INPUT','BUTTON','TEXTAREA','SELECT','FIELDSET','OUTPUT','OBJECT'].includes(target.tagName);
  const reportControlProperty=(target,property,previous)=>{
    if(!associated(target))return;
    const current=property==='checked'?Boolean(target.checked):String(target.value??'');
    if(current!==previous)globalThis.__lapui_record_page_change?.(target,property);
  };
  const valueMode=target=>target.tagName==='TEXTAREA'||target.tagName==='INPUT'&&textTypes.has(target.type);
  const sensitiveValue=target=>target.type==='password'||String(target.getAttribute('autocomplete')||'').split(/\s+/).some(token=>['current-password','new-password','one-time-code','cc-number','cc-csc','cc-exp','cc-exp-month','cc-exp-year'].includes(token.toLowerCase()));
  const state=target=>{let value=states.get(target);if(!value){value={dirty:false,checkedDirty:false,user:false,custom:''};states.set(target,value);}return value;};
  const numberPattern=/^-?(?:\d+(?:\.\d+)?|\.\d+)(?:[eE][+-]?\d+)?$/;
  const number=text=>numberPattern.test(text)&&Number.isFinite(Number(text))?Number(text):NaN;
  const selectOptions=select=>select.querySelectorAll('option');
  const owningSelect=option=>{for(let parent=option.parentElement;parent;parent=parent.parentElement)if(parent.tagName==='SELECT')return parent;return null;};
  const optionState=option=>{let value=optionStates.get(option);if(!value){const selected=option.hasAttribute('selected');value={default:selected,dirty:false};optionStates.set(option,value);}return value;};
  const setSelected=(select,option)=>{const options=selectOptions(select);if(!options.includes(option))return false;for(const candidate of options){optionState(candidate).dirty=true;if(candidate===option)oldSet.call(candidate,'selected','');else oldRemove.call(candidate,'selected');}return true;};
  Object.defineProperty(proto,'selected',{configurable:true,get(){return this.tagName==='OPTION'&&this.hasAttribute('selected');},set(value){
    if(this.tagName!=='OPTION')return;const select=owningSelect(this);
    if(Boolean(value)&&select&&!select.multiple)setSelected(select,this);
    else {optionState(this).dirty=true;if(Boolean(value))oldSet.call(this,'selected','');else oldRemove.call(this,'selected');}
  }});
  Object.defineProperty(proto,'defaultSelected',{configurable:true,get(){return this.tagName==='OPTION'&&optionState(this).default;},set(value){if(this.tagName==='OPTION'){const own=optionState(this);own.default=Boolean(value);if(!own.dirty){if(own.default)oldSet.call(this,'selected','');else oldRemove.call(this,'selected');}}}});
  Object.defineProperty(proto,'selectedIndex',{configurable:true,get(){if(this.tagName!=='SELECT')return undefined;const options=selectOptions(this),selected=options.findIndex(option=>option.selected);return selected>=0?selected:(options.length?0:-1);},set(value){if(this.tagName!=='SELECT')return;if(this.multiple)throw new DOMException('Multiple select is unsupported','NotSupportedError');value=Math.trunc(Number(value));const options=selectOptions(this);if(value<0||value>=options.length)throw new DOMException('Unmatched select index is unsupported','NotSupportedError');for(let index=0;index<options.length;index++)options[index].selected=index===value;}});
  Object.defineProperty(proto,'options',{configurable:true,get(){return this.tagName==='SELECT'?selectOptions(this):undefined;}});
  const sanitize=(target,value)=>{
    value=String(value);
    if(target.tagName==='TEXTAREA')return value.replace(/\r\n?/g,'\n');
    if(!textTypes.has(target.type))return value;
    value=value.replace(/[\r\n]/g,'');
    if(['email','url'].includes(target.type))value=value.replace(/^[\t\f ]+|[\t\f ]+$/g,'');
    if(target.type==='number'&&Number.isNaN(number(value)))return '';
    return value;
  };
  Object.defineProperty(proto,'type',{configurable:true,get(){
    const raw=(this.getAttribute('type')||'').toLowerCase();
    if(this.tagName==='INPUT')return knownTypes.has(raw)?raw:'text';
    if(this.tagName==='BUTTON')return ['submit','reset','button'].includes(raw)?raw:'submit';
    return raw;
  },set(value){this.setAttribute('type',String(value));}});
  const defaults=target=>target.tagName==='TEXTAREA'?oldText.get.call(target):target.getAttribute('value')||'';
  Object.defineProperty(proto,'value',{configurable:true,get(){
    if(this.tagName==='OPTION')return this.hasAttribute('value')?this.getAttribute('value'):this.textContent.trim().replace(/\s+/g,' ');
    if(this.tagName==='SELECT'){const options=selectOptions(this),selected=options.find(option=>option.selected)||options[0];return selected?selected.value:'';}
    if(!valueMode(this))return oldValue.get.call(this);
    const own=state(this);
    if(!own.dirty){const desired=sanitize(this,defaults(this));if(__lapui_get_value(this.__ref)!==desired)__lapui_set_value(this.__ref,desired);}
    return sanitize(this,__lapui_get_value(this.__ref));
  },set(value){
    if(this.tagName==='SELECT'){
      if(this.multiple)throw new DOMException('Multiple select is unsupported','NotSupportedError');
      const previous=String(this.value??'');
      const desired=String(value),options=selectOptions(this),option=options.find(item=>item.value===desired);
      if(!option)throw new DOMException('Unmatched select value is unsupported','NotSupportedError');
      setSelected(this,option);
      __lapui_set_value(this.__ref,desired);reportControlProperty(this,'value',previous);return;
    }
    if(this.tagName==='OPTION'){this.setAttribute('value',String(value));return;}
    if(this.tagName==='BUTTON'){this.setAttribute('value',String(value));return;}
    if(!valueMode(this)){
      const previous=String(this.value??'');
      oldValue.set.call(this,value);reportControlProperty(this,'value',previous);return;
    }
    const previous=String(this.value??'');
    const own=state(this);own.dirty=true;own.user=false;
    __lapui_set_value(this.__ref,sanitize(this,value));
    reportControlProperty(this,'value',previous);
  }});
  Object.defineProperty(proto,'defaultValue',{configurable:true,get(){return defaults(this);},set(value){
    if(this.tagName==='TEXTAREA')this.textContent=String(value);else this.setAttribute('value',String(value));
  }});
  Object.defineProperty(proto,'checked',{configurable:true,get:oldChecked.get,set(value){const previous=Boolean(oldChecked.get.call(this));state(this).checkedDirty=true;oldChecked.set.call(this,value);reportControlProperty(this,'checked',previous);}});
  Object.defineProperty(proto,'defaultChecked',{configurable:true,get(){return this.hasAttribute('checked');},set(value){if(value)this.setAttribute('checked','');else this.removeAttribute('checked');}});
  function changeAttribute(target,name,operation){
    name=String(name).toLowerCase();const own=state(target);
    const previous=valueMode(target)&&own.dirty&&name==='value'?__lapui_get_value(target.__ref):null;
    const checked=target.tagName==='INPUT'&&['checkbox','radio'].includes(target.type)&&name==='checked'?target.checked:null;
    operation(name);
    if(previous!==null)__lapui_set_value(target.__ref,previous);
    else if(valueMode(target)&&!own.dirty&&['type','value'].includes(name))__lapui_set_value(target.__ref,sanitize(target,defaults(target)));
    if(checked!==null){
      if(own.checkedDirty)__lapui_restore_checked(target.__ref,checked);
      else __lapui_set_checked(target.__ref,target.hasAttribute('checked'));
    }
  }
  proto.setAttribute=function(name,value){changeAttribute(this,name,normalized=>oldSet.call(this,normalized,value));};
  proto.removeAttribute=function(name){changeAttribute(this,name,normalized=>oldRemove.call(this,normalized));};
  for(const [name,original] of [['textContent',oldText],['innerHTML',oldHTML]])Object.defineProperty(proto,name,{configurable:true,get:original.get,set(value){
    original.set.call(this,value);
    if(this.tagName==='TEXTAREA'&&!state(this).dirty)__lapui_set_value(this.__ref,sanitize(this,defaults(this)));
  }});
  proto.cloneNode=function(deep=false){
    const copy=oldClone.call(this,deep);
    function transfer(source,target){
      if(valueMode(source)){const current=source.value;__lapui_set_value(target.__ref,current);states.set(target,{...state(source),user:false,custom:''});}
      else if(source.tagName==='INPUT'&&['checkbox','radio'].includes(source.type)){
        __lapui_restore_checked(target.__ref,source.checked);states.set(target,{...state(source),user:false,custom:''});
      }
      if(deep){const originals=source.childNodes,copies=target.childNodes;for(let i=0;i<originals.length;i++)transfer(originals[i],copies[i]);}
    }
    transfer(this,copy);return copy;
  };
  globalThis.__lapui_form_tree=target=>{
    const items=target.nodeType===1?[target,...target.querySelectorAll('input,textarea')]:[];
    for(const item of items)if(valueMode(item)&&!state(item).dirty)__lapui_set_value(item.__ref,sanitize(item,defaults(item)));
    const parent=target.parentElement;
    if(parent?.tagName==='TEXTAREA'&&!state(parent).dirty)__lapui_set_value(parent.__ref,sanitize(parent,defaults(parent)));
  };
  globalThis.__lapui_form_input=target=>{
    if(!associated(target))return;const own=state(target);
    if(valueMode(target)){own.dirty=true;own.user=true;}
    if(target.tagName==='INPUT'&&['checkbox','radio'].includes(target.type))own.checkedDirty=true;
  };
  Object.defineProperty(proto,'form',{configurable:true,get(){return associated(this)?node(__lapui_form_owner(this.__ref)):null;}});
  const controls=form=>__lapui_form_controls(form.__ref).map(node);
  const requireForm=form=>{if(!(form instanceof Element)||form.tagName!=='FORM')throw new TypeError('A form element is required');};
  for(const name of ['name','min','max','step','pattern','placeholder'])Object.defineProperty(proto,name,{configurable:true,get(){return this.getAttribute(name)||'';},set(value){this.setAttribute(name,String(value));}});
  for(const [name,attribute] of [['required','required'],['readOnly','readonly'],['multiple','multiple'],['noValidate','novalidate'],['formNoValidate','formnovalidate']])Object.defineProperty(proto,name,{configurable:true,get(){return this.hasAttribute(attribute);},set(value){if(value)this.setAttribute(attribute,'');else this.removeAttribute(attribute);}});
  for(const [name,attribute] of [['minLength','minlength'],['maxLength','maxlength']])Object.defineProperty(proto,name,{configurable:true,get(){const raw=this.getAttribute(attribute);return raw!==null&&/^\d+$/.test(raw)?Math.min(2147483647,Number(raw)):-1;},set(value){value=Math.trunc(Number(value));if(!Number.isFinite(value)||value<0)throw new DOMException('Length must be nonnegative','IndexSizeError');this.setAttribute(attribute,String(value));}});
  Object.defineProperty(proto,'valueAsNumber',{configurable:true,get(){return this.tagName==='INPUT'&&this.type==='number'?number(this.value):NaN;},set(value){
    if(this.tagName!=='INPUT'||this.type!=='number')throw new DOMException('Numeric value is unavailable','InvalidStateError');
    value=Number(value);if(!Number.isFinite(value)&&!Number.isNaN(value))throw new TypeError('Number must be finite');this.value=Number.isNaN(value)?'':String(value);
  }});
  function candidate(target){
    if(!associated(target)||!__lapui_is_enabled(target.__ref))return false;
    if(['FIELDSET','OUTPUT','OBJECT'].includes(target.tagName))return false;
    if(target.tagName==='INPUT'&&['hidden','button','reset'].includes(target.type)||target.tagName==='BUTTON'&&['button','reset'].includes(target.type))return false;
    if((valueMode(target)||target.tagName==='TEXTAREA')&&(target.readOnly||target.getAttribute('aria-readonly')==='true'))return false;
    for(let parent=target.parentElement;parent;parent=parent.parentElement)if(parent.tagName==='DATALIST')return false;
    return true;
  }
  const flags=['valueMissing','typeMismatch','patternMismatch','tooLong','tooShort','rangeUnderflow','rangeOverflow','stepMismatch','badInput','customError'];
  function validation(target){
    if(!associated(target))throw new TypeError('A form control is required');
    const own=state(target), result=Object.fromEntries(flags.map(name=>[name,false]));
    result.customError=Boolean(own.custom);
    if(candidate(target)){
      if(target.tagName==='SELECT'&&target.multiple)throw new DOMException('Multiple select is unsupported','NotSupportedError');
      if(target.tagName==='INPUT'&&!textTypes.has(target.type)&&!['checkbox','radio','submit','image'].includes(target.type))throw new DOMException('Validation for this native control type is not implemented','NotSupportedError');
      const value=target.value, kind=target.type;
      if(target.required){
        if(kind==='checkbox')result.valueMissing=!target.checked;
        else if(kind==='radio'){
          const group=__lapui_radio_group(target.__ref).split('\n').filter(Boolean).map(node);
          result.valueMissing=group.some(input=>input.required)&&!group.some(input=>input.checked);
        }else if(valueMode(target))result.valueMissing=value==='';
      }else if(kind==='radio'){
        const group=__lapui_radio_group(target.__ref).split('\n').filter(Boolean).map(node);
        result.valueMissing=group.some(input=>input.required)&&!group.some(input=>input.checked);
      }
      if(value&&kind==='email'){
        const email=/^[a-zA-Z0-9.!#$%&'*+\/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?)*$/;
        const values=target.multiple?value.split(',').map(part=>part.trim()):[value];
        result.typeMismatch=values.some(part=>!email.test(part));
      }
      if(value&&kind==='url')result.typeMismatch=!__lapui_url_valid(value);
      if(value&&target.tagName==='INPUT'&&['text','search','tel','url','email','password'].includes(kind)&&target.hasAttribute('pattern')){
        const pattern=target.pattern;
        if(pattern.length>limits.patternCodeUnits||value.length>limits.patternValueCodeUnits)throw new RangeError('Pattern validation input exceeds preview limits');
        try{const expression=new RegExp(`^(?:${pattern})$`,'v');const values=kind==='email'&&target.multiple?value.split(',').map(part=>part.trim()):[value];result.patternMismatch=values.some(part=>!expression.test(part));}
        catch(error){if(!(error instanceof SyntaxError))throw error;}
      }
      if(valueMode(target)&&own.dirty&&own.user&&value){
        result.tooLong=target.maxLength>=0&&value.length>target.maxLength;
        result.tooShort=target.minLength>=0&&value.length<target.minLength;
      }
      if(kind==='number'){
        const raw=__lapui_get_value(target.__ref), current=number(value), min=number(target.min), max=number(target.max);
        result.badInput=own.user&&raw!==''&&Number.isNaN(number(raw));
        if(value){
          result.rangeUnderflow=Number.isFinite(min)&&current<min;result.rangeOverflow=Number.isFinite(max)&&current>max;
          if(target.step!=='any'){
            const declared=number(target.step), step=declared>0?declared:1, declaredValue=number(target.getAttribute('value')||'');
            const base=Number.isFinite(min)?min:Number.isFinite(declaredValue)?declaredValue:0;
            const quotient=(current-base)/step;
            result.stepMismatch=Number.isFinite(quotient)&&Math.abs(quotient-Math.round(quotient))>1e-7*Math.max(1,Math.abs(quotient));
          }
        }
      }
    }
    result.valid=!flags.some(name=>result[name]);return result;
  }
  class ValidityState{constructor(){throw new TypeError('Illegal constructor');}}
  Object.defineProperty(proto,'validity',{configurable:true,get(){
    if(!associated(this))return undefined;
    let view=validityViews.get(this);if(!view){view=Object.create(ValidityState.prototype);for(const key of [...flags,'valid'])Object.defineProperty(view,key,{enumerable:true,get:()=>validation(this)[key]});validityViews.set(this,view);}return view;
  }});
  Object.defineProperty(proto,'willValidate',{configurable:true,get(){return candidate(this);}});
  const message=target=>{
    if(!candidate(target))return '';const current=validation(target);if(current.valid)return '';
    if(current.customError)return state(target).custom;
    if(current.valueMissing)return 'Please fill out this field.';
    if(current.typeMismatch)return 'Please enter a valid '+target.type+'.';
    if(current.patternMismatch)return 'Please match the requested format.';
    if(current.tooLong)return 'Please shorten this value.';if(current.tooShort)return 'Please lengthen this value.';
    if(current.rangeUnderflow)return 'The value is below the minimum.';if(current.rangeOverflow)return 'The value is above the maximum.';
    if(current.stepMismatch)return 'Please enter a permitted step value.';return 'Please enter a number.';
  };
  Object.defineProperty(proto,'validationMessage',{configurable:true,get(){return associated(this)?message(this):undefined;}});
  proto.setCustomValidity=function(value){if(!associated(this))throw new TypeError('A form control is required');value=String(value);if(value.length>limits.customMessageCodeUnits)throw new RangeError('Custom validation message exceeds preview limits');state(this).custom=value;};
  function check(target,report){
    const items=target.tagName==='FORM'?controls(target):[target];let valid=true,focus=null;
    for(const item of items){
      if(!candidate(item)||validation(item).valid)continue;valid=false;
      const outcome=JSON.parse(__lapui_dispatch('invalid',item.__ref,__lapui_event_path(item.__ref)));
      if(report&&!outcome.defaultPrevented&&!focus)focus=item;
    }
    if(focus)focus.focus();return valid;
  }
  proto.checkValidity=function(){if(this.tagName!=='FORM'&&!associated(this))throw new TypeError('A form or control is required');return check(this,false);};
  proto.reportValidity=function(){if(this.tagName!=='FORM'&&!associated(this))throw new TypeError('A form or control is required');return check(this,true);};
  class HTMLFormControlsCollection{constructor(){throw new TypeError('Illegal constructor');}}
  const matches=(form,name)=>controls(form).filter(item=>item.id===name||item.name===name);
  Object.defineProperty(proto,'elements',{configurable:true,get(){
    if(this.tagName!=='FORM')return undefined;
    let collection=collections.get(this);if(collection)return collection;
    const form=this, target=Object.create(HTMLFormControlsCollection.prototype);
    Object.defineProperties(target,{
      length:{get:()=>controls(form).length},item:{value:index=>controls(form)[Number(index)>>>0]??null},
      namedItem:{value:name=>{const found=matches(form,String(name));if(found.length<=1)return found[0]??null;
        return {get length(){return matches(form,String(name)).length;},item(index){return matches(form,String(name))[Number(index)>>>0]??null;},get value(){return matches(form,String(name)).find(item=>item.type==='radio'&&item.checked)?.value||'';},set value(value){const radio=matches(form,String(name)).find(item=>item.type==='radio'&&item.value===String(value));if(radio)radio.checked=true;},[Symbol.iterator](){return matches(form,String(name))[Symbol.iterator]();}};}},
      [Symbol.iterator]:{value:()=>controls(form)[Symbol.iterator]()}
    });
    collection=new Proxy(target,{get(object,key,receiver){if(Reflect.has(object,key))return Reflect.get(object,key,receiver);if(typeof key==='string')return /^\d+$/.test(key)?controls(form)[Number(key)]:object.namedItem(key)??undefined;}});
    collections.set(this,collection);return collection;
  }});
  Object.defineProperty(proto,'length',{configurable:true,get(){return this.tagName==='FORM'?controls(this).length:undefined;}});
  const submitButton=target=>target instanceof Element&&(target.tagName==='BUTTON'&&target.type==='submit'||target.tagName==='INPUT'&&['submit','image'].includes(target.type));
  function validateSubmitter(form,submitter){
    if(submitter==null)return;if(!submitButton(submitter))throw new TypeError('Submitter must be a submit button');
    if(submitter.form!==form)throw new DOMException('Submitter belongs to another form','NotFoundError');
  }
  proto.requestSubmit=function(submitter=null){
    requireForm(this);validateSubmitter(this,submitter);if(busySubmit.has(this)||!this.isConnected)return;
    busySubmit.add(this);
    try{
      if(!this.noValidate&&!submitter?.formNoValidate&&!check(this,true))return;
      const result=JSON.parse(__lapui_dispatch('submit',this.__ref,__lapui_event_path(this.__ref), '{}', {submitter}));
      if(!result.defaultPrevented&&this.isConnected)throw new DOMException('Handle local form submission with preventDefault; navigation is not implemented','NotSupportedError');
    }finally{busySubmit.delete(this);}
  };
  proto.submit=function(){requireForm(this);throw new DOMException('Form navigation is not implemented','NotSupportedError');};
  proto.reset=function(){
    requireForm(this);if(busyReset.has(this))return;busyReset.add(this);
    try{
      if(JSON.parse(__lapui_dispatch('reset',this.__ref,__lapui_event_path(this.__ref))).defaultPrevented)return;
      const items=controls(this);
      if(items.some(item=>item.tagName==='INPUT'&&['file','date','month','week','time','datetime-local','range','color'].includes(item.type)))throw new DOMException('Reset for this native control type is not implemented','NotSupportedError');
      for(const item of items){
        const own=state(item);own.dirty=false;own.user=false;own.checkedDirty=false;
        if(valueMode(item))__lapui_set_value(item.__ref,sanitize(item,defaults(item)));
        else if(item.tagName==='SELECT'){
          const options=selectOptions(item);
          for(const option of options){const own=optionState(option);if(own.default)oldSet.call(option,'selected','');else oldRemove.call(option,'selected');own.dirty=false;}
          if(!item.multiple&&!options.some(option=>option.selected)&&options[0])oldSet.call(options[0],'selected','');
        }
        else if(item.tagName==='INPUT'&&['checkbox','radio'].includes(item.type))__lapui_set_checked(item.__ref,item.defaultChecked);
      }
    }finally{busyReset.delete(this);}
  };
  const wellFormed=value=>String(value).replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g,'\uFFFD');
  function bound(entries){if(entries.length>limits.formDataEntries||entries.reduce((size,pair)=>size+pair[0].length+pair[1].length,0)>limits.formDataCodeUnits)throw new RangeError('FormData exceeds preview limits');}
  class FormData{
    constructor(form,submitter=null){
      dataState.set(this,[]);if(form===undefined)return;requireForm(form);validateSubmitter(form,submitter);
      for(const item of controls(form)){
        if(!__lapui_is_enabled(item.__ref)||!item.name||['FIELDSET','OUTPUT','OBJECT'].includes(item.tagName))continue;
        let inDatalist=false;for(let parent=item.parentElement;parent;parent=parent.parentElement)if(parent.tagName==='DATALIST')inDatalist=true;
        if(inDatalist||item.tagName==='BUTTON'&&item!==submitter||item.tagName==='INPUT'&&['button','reset','submit','image'].includes(item.type)&&item!==submitter)continue;
        if(item.tagName==='INPUT'&&['checkbox','radio'].includes(item.type)&&!item.checked)continue;
        if(item.tagName==='SELECT'&&item.multiple||item.tagName==='INPUT'&&['file','image'].includes(item.type))throw new DOMException('This native control has no supported FormData entry','NotSupportedError');
        this.append(item.name,item.value);
      }
      __lapui_dispatch('formdata',form.__ref,__lapui_event_path(form.__ref),'{}',{formData:this});
    }
    append(name,value,filename){if(filename!==undefined||typeof Blob!=='undefined'&&value instanceof Blob)throw new DOMException('Binary FormData is not implemented','NotSupportedError');const entries=dataState.get(this),next=[...entries,[wellFormed(name),wellFormed(value)]];bound(next);dataState.set(this,next);}
    set(name,value,filename){if(filename!==undefined||typeof Blob!=='undefined'&&value instanceof Blob)throw new DOMException('Binary FormData is not implemented','NotSupportedError');name=wellFormed(name);value=wellFormed(value);const entries=dataState.get(this),first=entries.findIndex(pair=>pair[0]===name),next=entries.filter(pair=>pair[0]!==name);next.splice(first<0?next.length:first,0,[name,value]);bound(next);dataState.set(this,next);}
    delete(name){name=wellFormed(name);dataState.set(this,dataState.get(this).filter(pair=>pair[0]!==name));}
    get(name){name=wellFormed(name);return dataState.get(this).find(pair=>pair[0]===name)?.[1]??null;}
    getAll(name){name=wellFormed(name);return dataState.get(this).filter(pair=>pair[0]===name).map(pair=>pair[1]);}
    has(name){name=wellFormed(name);return dataState.get(this).some(pair=>pair[0]===name);}
    *entries(){for(let index=0;index<dataState.get(this).length;index++)yield [...dataState.get(this)[index]];}
    *keys(){for(const [name] of this.entries())yield name;}
    *values(){for(const [,value] of this.entries())yield value;}
    [Symbol.iterator](){return this.entries();}
    forEach(callback,thisArg){if(typeof callback!=='function')throw new TypeError('Callback must be a function');for(const [name,value] of this.entries())callback.call(thisArg,value,name,this);}
  }
  globalThis.__lapui_form_click=(target,path,event)=>{
    if(target?.tagName==='OPTION'&&!event.defaultPrevented){
      const select=owningSelect(target);
      if(select&&__lapui_is_enabled(select.__ref)){
        if(select.multiple)throw new DOMException('Multiple select is unsupported','NotSupportedError');
        const changed=!target.selected;setSelected(select,target);
        select.focus();
        if(changed)for(const type of ['input','change'])__lapui_dispatch(type,select.__ref,__lapui_event_path(select.__ref));
      }
    }
    const button=path.find(item=>item?.tagName==='BUTTON'||item?.tagName==='INPUT'&&['submit','reset','image'].includes(item.type));
    if(!button||!button.form||!['submit','reset','image'].includes(button.type))return false;
    if(!event.defaultPrevented&&__lapui_is_enabled(button.__ref)){
      if(button.type==='reset')button.form.reset();else button.form.requestSubmit(button);
    }
    return true;
  };
  globalThis.__lapui_form_enter=(target,event)=>{
    if(!target||target.tagName!=='INPUT'||!textTypes.has(target.type)||!target.form||!__lapui_is_enabled(target.__ref))return false;
    const form=target.form, items=controls(form), submitter=items.find(submitButton);
    if(event.defaultPrevented||event.repeat||event.isComposing)return true;
    if(submitter){if(__lapui_is_enabled(submitter.__ref))submitter.click();}
    else if(items.filter(item=>item.tagName==='INPUT'&&textTypes.has(item.type)).length<=1)form.requestSubmit();
    return true;
  };
  const previousControls=lapui.controls;
  globalThis.__lapui_form_snapshot=snapshot=>{
    for(const item of snapshot.controls){
      const target=node(item.ref);if(!target||!associated(target))continue;
      item.formRef=target.form?.__ref??null;
      try{
        item.willValidate=candidate(target);item.validity=validation(target);
        if(!sensitiveValue(target))item.validationMessage=message(target);
        if(valueMode(target)&&!sensitiveValue(target))item.value=target.value;
      }catch(error){if(error.name!=='NotSupportedError')throw error;item.validationAvailable=false;}
    }
    return snapshot;
  };
  globalThis.__lapui_form_snapshot_json=json=>JSON.stringify(__lapui_form_snapshot(JSON.parse(json)));
  lapui.controls=()=>__lapui_form_snapshot(previousControls());
  __lapui_form_tree(document.documentElement);
  Object.assign(globalThis,{FormData,ValidityState,HTMLFormControlsCollection});
  for(const [name,tag] of [['HTMLFormElement','FORM'],['HTMLInputElement','INPUT'],['HTMLTextAreaElement','TEXTAREA'],['HTMLButtonElement','BUTTON']])globalThis[name]=class {constructor(){throw new TypeError('Illegal constructor');}static [Symbol.hasInstance](value){return value instanceof Element&&value.tagName===tag;}};
})();
