// ESM module for testing Strobe ESM tracing.
// Has a setInterval to keep alive for Frida attach + pattern install.

export function handleRequest(method, path) {
  console.log(`ESM: ${method} ${path}`);
  return { status: 200 };
}

export function processData(input) {
  return input.map(x => x * 2);
}

console.log("esm_target: starting");

// Call functions on a timer — gives Frida time to attach and install hooks
let count = 0;
const timer = setInterval(() => {
  handleRequest("GET", `/api/test/${count}`);
  processData([1, 2, 3]);
  count++;
  if (count >= 5) {
    clearInterval(timer);
    console.log("esm_target: done");
  }
}, 500);
