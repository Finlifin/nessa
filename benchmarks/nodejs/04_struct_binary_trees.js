"use strict";
const { performance } = require("perf_hooks");

class TreeNode {
  constructor(item, left, right) {
    this.item = item;
    this.left = left;
    this.right = right;
  }
}

function makeTree(item, depth) {
  if (depth <= 0) {
    return new TreeNode(item, null, null);
  }
  const nextDepth = depth - 1;
  return new TreeNode(
    item,
    makeTree(2 * item - 1, nextDepth),
    makeTree(2 * item, nextDepth)
  );
}

function checkTree(node) {
  if (node.left === null) return node.item;
  return node.item + checkTree(node.left) - checkTree(node.right);
}

function runTrees() {
  const minDepth = 4;
  const maxDepth = 13;
  const stretch = makeTree(0, maxDepth + 1);
  let totalCheck = checkTree(stretch);
  const longLived = makeTree(0, maxDepth);

  for (let depth = minDepth; depth <= maxDepth; depth += 2) {
    const iterations = 1 << (maxDepth - depth + minDepth);
    let chk = 0;
    for (let i = 1; i <= iterations; i++) {
      chk += checkTree(makeTree(i, depth));
      chk += checkTree(makeTree(-i, depth));
    }
    totalCheck += chk;
  }
  return totalCheck + checkTree(longLived);
}

const t0 = performance.now();
const ans = runTrees();
const ms = performance.now() - t0;
console.log("KERNEL_MS:" + ms.toFixed(6));
console.log("CHECKSUM:" + ans);
