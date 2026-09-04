package com.example

import com.example.Util

fun run(n: Int): Int {
    val u = Util()
    return u.send(n)
}

fun viaAscription(n: Int): Int {
    val u: Util = Util()
    return u.send(n)
}

fun viaParam(u: Util, n: Int): Int {
    return u.send(n)
}
