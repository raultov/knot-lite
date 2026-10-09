import { discountedPrice } from "./pricing.js";

/** A shopping cart. */
export class Cart {
  constructor() {
    this.lines = [];
  }

  add(name, unitPrice, quantity) {
    this.lines.push({ name, unitPrice, quantity });
  }

  total(rate) {
    let sum = 0;
    for (const line of this.lines) {
      sum += discountedPrice(line.unitPrice, line.quantity, rate);
    }
    return sum;
  }
}
