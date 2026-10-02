// Shared by the browser reference generator and the Rust regression.
const ids = ['root', 'row', 'left', 'fill', 'badge', 'scroll', 'content', 'child'];
function measure() {
  const boxes = {};
  for (const id of ids) {
    const r = document.getElementById(id).getBoundingClientRect();
    const fragments = document.getElementById(id).getClientRects();
    if (fragments.length !== 1 || ['x','y','width','height'].some(
      key => Math.abs(fragments[0][key] - r[key]) > 0.02)) {
      throw new Error('Box fragment disagrees with bounding rect: ' + id);
    }
    boxes[id] = {x:r.x, y:r.y, width:r.width, height:r.height};
  }
  const s = document.getElementById('scroll');
  return {boxes, scroll:{left:s.scrollLeft, top:s.scrollTop,
    clientWidth:s.clientWidth, clientHeight:s.clientHeight,
    scrollWidth:s.scrollWidth, scrollHeight:s.scrollHeight}};
}
const cases = [];
for (const size of [[800,600], [640,480]]) {
  const root = document.getElementById('root');
  root.style.width = size[0] + 'px'; root.style.height = size[1] + 'px';
  const scroll = document.getElementById('scroll');
  scroll.scrollLeft = 0; scroll.scrollTop = 0;
  const before = measure();
  // Integral physical offsets at both 1x and 1.5x; scroll quantization differs
  // across engines and is outside this geometry fixture's scope.
  scroll.scrollLeft = 28; scroll.scrollTop = 40;
  cases.push({width:size[0], height:size[1], before, after:measure()});
}
