package com.example.shop.api;

import com.example.shop.model.Item;
import com.example.shop.model.Order;
import com.example.shop.service.PriceCalculator;

/** Entry point of the checkout flow. */
public class CheckoutController {
    private final PriceCalculator calculator;

    public CheckoutController(PriceCalculator calculator) {
        this.calculator = calculator;
    }

    public long checkout(Order order) {
        return calculator.total(order);
    }

    public int count(Order order) {
        return order.getItems().size();
    }

    public Order single(String name, long cents) {
        Order order = new Order();
        order.addItem(new Item(name, cents, 1));
        return order;
    }
}
