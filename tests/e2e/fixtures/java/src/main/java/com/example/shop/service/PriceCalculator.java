package com.example.shop.service;

import com.example.shop.model.Order;

/** Prices an order. */
public interface PriceCalculator {
    long total(Order order);
}
