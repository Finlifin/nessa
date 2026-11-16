"use strict";
const { performance } = require("perf_hooks");

function runStringBench(lines) {
  let totalLatency = 0;
  let totalBytes = 0;
  let matchedOk = 0;
  for (let i = 0; i < lines; i++) {
    const lat = (i * 37 + 13) % 997;
    const code = i % 5 === 0 ? "500" : "200";
    const line =
      "2026-11-17T12:00:00Z service=gateway req=" +
      i +
      " latency=" +
      lat +
      "ms status=" +
      code;
    totalBytes += line.length;
    if (line.endsWith("status=200")) {
      matchedOk += 1;
    }
    const pos = line.indexOf("latency=");
    if (pos >= 0) {
      const start = pos + 8;
      const endPos = line.indexOf("ms status=");
      if (endPos >= 0) {
        totalLatency += parseInt(line.slice(start, endPos), 10);
      }
    }
  }
  return totalLatency * 100 + matchedOk + (totalBytes % 100000);
}

const t0 = performance.now();
const ans = runStringBench(25000);
const ms = performance.now() - t0;
console.log("KERNEL_MS:" + ms.toFixed(6));
console.log("CHECKSUM:" + ans);
