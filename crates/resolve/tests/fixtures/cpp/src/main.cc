#include "app.hpp"

int run(int n) {
    Foo f;
    Other o;
    f.count = n;
    f.bar();
    o.bar();
    return f.count + free_helper(n);
}
