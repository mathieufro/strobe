// Bun app with multiple named functions for multi-hook attribution testing.

function handleRequest(method: string, path: string) {
  console.log(`Request: ${method} ${path}`);
  return { status: 200, body: "ok" };
}

function processData(input: number[]): number[] {
  return input.map(x => x * 2);
}

function computeHash(data: string): number {
  let hash = 0;
  for (let i = 0; i < data.length; i++) {
    hash = ((hash << 5) - hash) + data.charCodeAt(i);
    hash |= 0;
  }
  return hash;
}

console.log("bun_inspector_target: starting");
setTimeout(() => {
  handleRequest("GET", "/api/test");
  processData([1, 2, 3]);
  computeHash("hello world");
  console.log("bun_inspector_target: done");
}, 1000);

// Keep alive for 10s to give validation script ample time
setTimeout(() => process.exit(0), 10000);
