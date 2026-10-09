/** Sum of prices, kept for CommonJS callers. */
function legacyTotal(prices) {
  let sum = 0;
  for (const price of prices) {
    sum += price;
  }
  return sum;
}

/** One-line report of the legacy total. */
function legacyReport(prices) {
  return `total=${legacyTotal(prices)}`;
}

module.exports = { legacyTotal, legacyReport };
