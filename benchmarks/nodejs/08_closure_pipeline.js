"use strict";
const { performance } = require("perf_hooks");

function mapI64(src, f) {
  const out = [];
  const n = src.length;
  for (let i = 0; i < n; i++) {
    out.push(f(src[i]));
  }
  return out;
}

function filterI64(src, pred) {
  const out = [];
  const n = src.length;
  for (let i = 0; i < n; i++) {
    const v = src[i];
    if (pred(v)) out.push(v);
  }
  return out;
}

function foldI64(src, init, step) {
  let acc = init;
  const n = src.length;
  for (let i = 0; i < n; i++) {
    acc = step(acc, src[i]);
  }
  return acc;
}

function runClosureBench(size, rounds) {
  const base = [];
  for (let i = 0; i < size; i++) {
    base.push((i * 17 + 5) % 1000);
  }
  let grandTotal = 0;
  for (let r = 0; r < rounds; r++) {
    const offset = r * 3 + 7;
    const scale = (r % 5) + 2;
    const mapped = mapI64(base, (x) => x * scale + offset);
    const filtered = filterI64(mapped, (x) => x % 3 !== 0);
    const folded = foldI64(filtered, 0, (acc, x) => (acc + x + offset) % 1000000007);
    grandTotal = (grandTotal + folded) % 1000000007;
  }
  return grandTotal;
}

const t0 = performance.now();
const ans = runClosureBench(10000, 25);
const ms = performance.now() - t0;
console.log("KERNEL_MS:" + ms.toFixed(6));
console.log("CHECKSUM:" + ans);
