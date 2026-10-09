package com.example.shop.model;

import java.util.ArrayList;
import java.util.List;

/** A customer order. */
public class Order {
    private final List<Item> items = new ArrayList<>();

    public void addItem(Item item) {
        items.add(item);
    }

    public List<Item> getItems() {
        return items;
    }
}
