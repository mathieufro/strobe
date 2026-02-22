// Target script for Bun multi-hook attribution test.
// Two distinct functions registered as separate timer callbacks — ensures
// JSObjectCallAsFunction fires for each (called from Bun's native event loop).

function processData(): void {
  let sum = 0;
  for (let i = 0; i < 10; i++) sum += i;
}

function validateInput(): void {
  const ok = Math.random() > 0;
  void ok;
}

export { processData, validateInput };

console.log("bun_multi_hook: starting");

// Register each function as a SEPARATE timer callback so the native event loop
// calls each one via JSObjectCallAsFunction (not JS-to-JS calls within a single callback)
const t1 = setInterval(processData, 200);
const t2 = setInterval(validateInput, 200);

setTimeout(() => {
  clearInterval(t1);
  clearInterval(t2);
  console.log("bun_multi_hook: done");
}, 20000);
