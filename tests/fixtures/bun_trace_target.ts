// Target script for Bun function tracing integration tests
let counter = 0;

function increment(n: number): number {
  counter += n;
  return counter;
}

class Calculator {
  add(a: number, b: number): number { return a + b; }
  async asyncAdd(a: number, b: number): Promise<number> {
    await new Promise(r => setTimeout(r, 1));
    return a + b;
  }
}

export { increment, Calculator };

// Keep running so Frida can attach and trace
const calc = new Calculator();
setInterval(() => {
  increment(1);
  calc.add(counter, 1);
}, 100);
