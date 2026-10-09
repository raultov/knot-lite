package com.example.gradle;

/** Command-line entry point. */
public class App {
    public static void main(String[] args) {
        System.out.println(new Greeter().greet(args.length > 0 ? args[0] : "world"));
    }
}
