"use strict";
const { performance } = require("perf_hooks");

function quicksort(arr, lo, hi) {
  if (lo >= hi) return;
  const mid = lo + Math.floor((hi - lo) / 2);
  const pivot = arr[mid];
  let i = lo;
  let j = hi;
  while (i <= j) {
    while (arr[i] < pivot) i += 1;
    while (arr[j] > pivot) j -= 1;
    if (i <= j) {
      const tmp = arr[i];
      arr[i] = arr[j];
      arr[j] = tmp;
      i += 1;
      j -= 1;
    }
  }
  if (lo < j) quicksort(arr, lo, j);
  if (i < hi) quicksort(arr, i, hi);
}

function runQuicksortBench(n, rounds) {
  let seed = 1234567;
  let checksum = 0;
  for (let r = 0; r < rounds; r++) {
    const arr = [];
    for (let k = 0; k < n; k++) {
      seed = (seed * 48271 + 12345) % 1000000007;
      arr.push(seed % 1000000);
    }
    quicksort(arr, 0, n - 1);
    checksum += arr[0];
    checksum += arr[Math.floor(n / 2)];
    checksum += arr[n - 1];
  }
  return checksum;
}

const t0 = performance.now();
const ans = runQuicksortBench(20000, 8);
const ms = performance.now() - t0;
console.log("KERNEL_MS:" + ms.toFixed(6));
console.log("CHECKSUM:" + ans);
