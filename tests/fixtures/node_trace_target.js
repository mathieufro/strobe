// Target script for Node.js function tracing integration tests
let counter = 0;

function increment(n) {
  counter += n;
  return counter;
}

class Calculator {
  add(a, b) { return a + b; }
  async asyncAdd(a, b) {
    await new Promise(r => setTimeout(r, 1));
    return a + b;
  }
}

module.exports = { increment, Calculator };

// Keep running so Frida can attach and trace
if (require.main === module) {
  const calc = new Calculator();
  setInterval(() => {
    increment(1);
    calc.add(counter, 1);
  }, 100);
}
