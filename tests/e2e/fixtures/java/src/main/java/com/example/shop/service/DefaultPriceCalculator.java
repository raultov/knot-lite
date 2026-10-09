package com.example.shop.service;

import com.example.shop.model.Item;
import com.example.shop.model.Order;

/** Sums unit price times quantity. */
public class DefaultPriceCalculator implements PriceCalculator {
    @Override
    public long total(Order order) {
        long sum = 0;
        for (Item item : order.getItems()) {
            sum += item.unitCents() * item.quantity();
        }
        return sum;
    }
}
