import { Cart } from "./cart.js";
import { discountedPrice } from "./pricing.js";

/** Checkout a list of items with a member discount. */
export function checkout(items) {
  const cart = new Cart();
  for (const item of items) {
    cart.add(item.name, item.price, item.quantity);
  }
  return cart.total(0.1);
}

/** Quote one product without a cart. */
export function quote(price) {
  return discountedPrice(price, 1, 0);
}
