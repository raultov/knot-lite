package com.example.shop.model;

/** One line of an order. */
public record Item(String name, long unitCents, int quantity) {}
