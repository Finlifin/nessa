"use strict";
const { performance } = require("perf_hooks");

function runMapBench(numKeys, rounds) {
  const keys = [];
  for (let i = 0; i < numKeys; i++) {
    keys.push("word_key_" + i);
  }
  const mp = new Map();
  for (let r = 0; r < rounds; r++) {
    const delta = 1 + (r % 7);
    for (let idx = 0; idx < numKeys; idx++) {
      const k = keys[idx];
      const prev = mp.get(k);
      mp.set(k, prev === undefined ? delta : prev + delta);
    }
  }
  let sum = mp.size;
  for (let j = 0; j < numKeys; j++) {
    sum += mp.get(keys[j]) * ((j % 11) + 1);
  }
  return sum;
}

const t0 = performance.now();
const ans = runMapBench(1500, 100);
const ms = performance.now() - t0;
console.log("KERNEL_MS:" + ms.toFixed(6));
console.log("CHECKSUM:" + ans);
