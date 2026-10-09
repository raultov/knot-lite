/** Price of `quantity` items after the discount `rate` (0..1). */
export function discountedPrice(unitPrice, quantity, rate) {
  return unitPrice * quantity * (1 - rate);
}
