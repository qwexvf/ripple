package com.example

fun helper(n: Int): Int {
    return n + 1
}

class Util {
    fun send(n: Int): Int {
        return helper(n) + trim(n)
    }

    fun trim(n: Int): Int {
        return n
    }
}

class Rival {
    fun send(n: Int): Int {
        return n
    }

    fun trim(n: Int): Int {
        return -n
    }
}
