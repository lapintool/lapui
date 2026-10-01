const byId = id => document.getElementById(id);
const viewport = byId('viewport');
for (let index = 1; index <= 12; index++) {
  const row = document.createElement('button');
  row.id = 'row-' + index;
  row.className = 'row';
  row.textContent = 'Row ' + index + ' — select to update the shared interface';
  row.onclick = () => {
    document.querySelectorAll('.row').forEach(item => item.classList.remove('selected'));
    row.classList.add('selected');
    byId('selected').value = 'Row ' + index;
  };
  byId('rows').appendChild(row);
}
function measure() {
  byId('position').value = viewport.scrollLeft.toFixed(1) + ' / ' + viewport.scrollTop.toFixed(1);
  byId('client').value = viewport.clientWidth + ' / ' + viewport.clientHeight;
  byId('extent').value = viewport.scrollWidth + ' / ' + viewport.scrollHeight;
  byId('row-position').value = byId('row-12').getBoundingClientRect().y.toFixed(1);
}
viewport.addEventListener('scroll',measure);
byId('down').onclick = () => viewport.scrollBy({top:120});
byId('right').onclick = () => viewport.scrollBy({left:120});
byId('end').onclick = () => viewport.scrollTo({left:viewport.scrollWidth,top:viewport.scrollHeight});
byId('home').onclick = () => viewport.scrollTo(0,0);
requestAnimationFrame(measure);
