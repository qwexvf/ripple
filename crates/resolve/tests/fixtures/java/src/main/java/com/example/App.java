package com.example;

import com.example.Util;

public class App {
    private Util util;

    public int run(int n) {
        return Util.helper(n);
    }

    public int viaParam(Util u, int n) {
        return u.send(n);
    }

    public int viaField(int n) {
        return util.send(n);
    }

    public int viaLocal(int n) {
        Util local = new Util();
        return local.send(n);
    }

    public int viaUnknown(java.util.List<Util> xs, int n) {
        return xs.get(0).send(n);
    }
}
