"use strict";
const { performance } = require("perf_hooks");

function fib(n) {
  if (n < 2) return n;
  return fib(n - 1) + fib(n - 2);
}

const t0 = performance.now();
const ans = fib(34);
const ms = performance.now() - t0;
console.log("KERNEL_MS:" + ms.toFixed(6));
console.log("CHECKSUM:" + ans);
