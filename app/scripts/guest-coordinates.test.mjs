
import assert from 'node:assert/strict';
import test from 'node:test';
import { mapGuestPoint } from '../src/guest-coordinates.mjs';

const box = (left, top, width, height) => ({ left, top, width, height });

test('native size preserves exact pixels and page offsets', () => {
  assert.deepEqual(mapGuestPoint(110, 220, box(10, 20, 1080, 1920), 1080, 1920), { x: 100, y: 200 });
});
test('scaled portrait coordinates use the rendered content dimensions', () => {
  assert.deepEqual(mapGuestPoint(280, 500, box(10, 20, 540, 960), 1080, 1920), { x: 540, y: 960 });
});
test('left and right object-fit letterboxes never forward taps', () => {
  const target = box(0, 0, 1000, 1000);
  assert.equal(mapGuestPoint(218.7, 500, target, 1080, 1920), null);
  assert.equal(mapGuestPoint(781.25, 500, target, 1080, 1920), null);
  assert.deepEqual(mapGuestPoint(500, 500, target, 1080, 1920), { x: 540, y: 960 });
});
test('top and bottom object-fit letterboxes never forward taps', () => {
  const target = box(0, 0, 1000, 1000);
  assert.equal(mapGuestPoint(500, 218.7, target, 1920, 1080), null);
  assert.equal(mapGuestPoint(500, 781.25, target, 1920, 1080), null);
  assert.deepEqual(mapGuestPoint(500, 500, target, 1920, 1080), { x: 960, y: 540 });
});
test('top-left is included and bottom-right is excluded', () => {
  const target = box(100, 200, 1080, 1920);
  assert.deepEqual(mapGuestPoint(100, 200, target, 1080, 1920), { x: 0, y: 0 });
  assert.deepEqual(mapGuestPoint(1179.99, 2119.99, target, 1080, 1920), { x: 1079, y: 1919 });
  assert.equal(mapGuestPoint(1180, 500, target, 1080, 1920), null);
  assert.equal(mapGuestPoint(500, 2120, target, 1080, 1920), null);
});
test('zero hidden nonfinite and missing boxes are rejected', () => {
  for (const target of [null, box(0, 0, 0, 1), box(0, 0, 1, -1), box(NaN, 0, 1, 1), box(0, 0, Infinity, 1)]) {
    assert.equal(mapGuestPoint(0, 0, target, 1, 1), null);
  }
  assert.equal(mapGuestPoint(NaN, 0, box(0, 0, 1, 1), 1, 1), null);
  assert.equal(mapGuestPoint(0, Infinity, box(0, 0, 1, 1), 1, 1), null);
});
test('pixel dimensions must be integers within the backend limit', () => {
  for (const [width, height] of [[0, 1], [1, 0], [-1, 1], [1.5, 1], [1, 2.5], [32769, 1], [1, 32769], [NaN, 1]]) {
    assert.equal(mapGuestPoint(0, 0, box(0, 0, 10, 10), width, height), null);
  }
  assert.deepEqual(mapGuestPoint(0, 0, box(0, 0, 1, 1), 1, 1), { x: 0, y: 0 });
});
test('noninteger render sizes and extreme scale still yield bounded integer pixels', () => {
  const target = box(-20.25, 4.5, 377.375, 630.625);
  const actual = mapGuestPoint(target.left + target.width / 2, target.top + target.height / 2, target, 1080, 1920);
  assert(actual && Number.isInteger(actual.x) && Number.isInteger(actual.y));
  assert(actual.x >= 0 && actual.x < 1080 && actual.y >= 0 && actual.y < 1920);
  assert.deepEqual(mapGuestPoint(0, 0, box(0, 0, 1, 1), 32768, 32768), { x: 0, y: 0 });
});