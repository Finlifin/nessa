"use strict";
const { performance } = require("perf_hooks");

function runSieve(limit) {
  const flags = new Array(limit + 1).fill(true);
  flags[0] = false;
  flags[1] = false;
  let p = 2;
  while (p * p <= limit) {
    if (flags[p]) {
      let k = p * p;
      while (k <= limit) {
        flags[k] = false;
        k += p;
      }
    }
    p += 1;
  }
  let count = 0;
  for (let i = 2; i <= limit; i++) {
    if (flags[i]) count += 1;
  }
  return count;
}

function runLcg(steps, seed) {
  let state = seed;
  let acc = 0;
  for (let i = 0; i < steps; i++) {
    state = (state * 48271 + 12345) % 1000000007;
    acc = (acc + (state % 65536)) % 1000000007;
  }
  return acc;
}

const t0 = performance.now();
let primeSum = 0;
for (let r = 0; r < 40; r++) {
  primeSum += runSieve(50000);
}
const lcgAcc = runLcg(2000000, 42);
const checksum = primeSum * 1000 + (lcgAcc % 1000);
const ms = performance.now() - t0;
console.log("KERNEL_MS:" + ms.toFixed(6));
console.log("CHECKSUM:" + checksum);
