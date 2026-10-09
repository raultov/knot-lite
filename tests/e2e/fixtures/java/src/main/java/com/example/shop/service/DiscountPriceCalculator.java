package com.example.shop.service;

import com.example.shop.model.Order;

/** Applies a percentage discount on top of another calculator. */
public class DiscountPriceCalculator implements PriceCalculator {
    private final PriceCalculator base;
    private final int percent;

    public DiscountPriceCalculator(PriceCalculator base, int percent) {
        this.base = base;
        this.percent = percent;
    }

    @Override
    public long total(Order order) {
        return base.total(order) * (100 - percent) / 100;
    }
}
