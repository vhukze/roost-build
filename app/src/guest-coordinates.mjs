
/** Pure object-fit:contain, object-position:center mapping; no DOM or input side effects. */
export function mapGuestPoint(clientX, clientY, box, width, height) {
  if (!box || ![clientX, clientY, box.left, box.top, box.width, box.height].every(Number.isFinite)
      || box.width <= 0 || box.height <= 0
      || !Number.isInteger(width) || !Number.isInteger(height)
      || width < 1 || height < 1 || width > 32768 || height > 32768) return null;
  const scale = Math.min(box.width / width, box.height / height);
  const drawnWidth = width * scale, drawnHeight = height * scale;
  const left = box.left + (box.width - drawnWidth) / 2;
  const top = box.top + (box.height - drawnHeight) / 2;
  if (!Number.isFinite(scale) || scale <= 0 || ![left, top, drawnWidth, drawnHeight].every(Number.isFinite)
      || clientX < left || clientY < top || clientX >= left + drawnWidth || clientY >= top + drawnHeight) return null;
  return {
    x: Math.max(0, Math.min(width - 1, Math.floor((clientX - left) / scale))),
    y: Math.max(0, Math.min(height - 1, Math.floor((clientY - top) / scale))),
  };
}