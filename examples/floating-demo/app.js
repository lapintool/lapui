import {computePosition,autoUpdate,offset,flip,shift} from '@floating-ui/dom';
const byId = id => document.getElementById(id);
const anchor = byId('anchor'), popover = byId('popover'), stage = byId('stage');
let opened = false, revision = 0, stopUpdates = null;
const style = getComputedStyle(popover);
async function position() {
  const current = ++revision;
  if (!opened) return;
  byId('status').value = 'Positioning';
  try {
    const result = await computePosition(anchor,popover,{placement:'bottom-start',middleware:[offset(8),flip({padding:8}),shift({padding:8})]});
    if (!opened || current !== revision) return;
    popover.style.left = result.x + 'px';
    popover.style.top = result.y + 'px';
    byId('placement').value = result.placement;
    byId('coordinates').value = result.x.toFixed(1) + ' / ' + result.y.toFixed(1);
    byId('style-width').value = style.width;
    const rect = popover.getBoundingClientRect(), container = stage.getBoundingClientRect();
    const inside = rect.left >= container.left + stage.clientLeft - .5 && rect.top >= container.top + stage.clientTop - .5
      && rect.right <= container.left + stage.clientLeft + stage.clientWidth + .5 && rect.bottom <= container.top + stage.clientTop + stage.clientHeight + .5;
    byId('bounds').value = inside ? 'Yes' : 'No';
    byId('status').value = 'Open';
  } catch(error) {
    if (current === revision) byId('status').value = error.name + ': ' + error.message;
  }
}
function close(status = 'Closed') {
  opened = false; revision++;
  stopUpdates?.(); stopUpdates = null;
  popover.style.display = 'none';
  byId('status').value = status;
  byId('placement').value = 'None';
  byId('bounds').value = 'Hidden';
}
anchor.onclick = () => {
  opened = true; popover.style.display = 'block';
  stopUpdates?.();
  stopUpdates = autoUpdate(anchor,popover,position);
};
byId('center').onclick = () => { anchor.style.left='24px'; anchor.style.top='100px'; position(); };
byId('edge').onclick = () => { anchor.style.left='calc(100% - 110px)'; anchor.style.top='220px'; position(); };
byId('left-edge').onclick = () => { anchor.style.left='-60px'; anchor.style.top='100px'; position(); };
byId('reposition').onclick = position;
byId('close').onclick = () => close();
byId('accept').onclick = () => close('Accepted');

let wide = false;
byId('resize-anchor').onclick = () => { wide=!wide; anchor.style.width=(wide ? 160 : 100)+'px'; };
