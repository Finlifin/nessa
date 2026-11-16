"use strict";
const { performance } = require("perf_hooks");

function mandelbrot(size, maxIter) {
  let totalIters = 0;
  for (let y = 0; y < size; y++) {
    const ci = (2.0 * y) / size - 1.0;
    for (let x = 0; x < size; x++) {
      const cr = (2.0 * x) / size - 1.5;
      let zr = 0.0;
      let zi = 0.0;
      let iter = 0;
      while (iter < maxIter) {
        const zr2 = zr * zr;
        const zi2 = zi * zi;
        if (zr2 + zi2 > 4.0) break;
        zi = 2.0 * zr * zi + ci;
        zr = zr2 - zi2 + cr;
        iter += 1;
      }
      totalIters += iter;
    }
  }
  return totalIters;
}

const t0 = performance.now();
const ans = mandelbrot(400, 100);
const ms = performance.now() - t0;
console.log("KERNEL_MS:" + ms.toFixed(6));
console.log("CHECKSUM:" + ans);
