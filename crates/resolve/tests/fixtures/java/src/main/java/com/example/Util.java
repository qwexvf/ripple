package com.example;

public class Util {
    public static int helper(int n) {
        return n + 1;
    }

    public int send(int n) {
        return trim(n);
    }

    public int trim(int n) {
        return n;
    }
}

class Rival {
    public int send(int n) {
        return n;
    }

    public int trim(int n) {
        return -n;
    }
}
