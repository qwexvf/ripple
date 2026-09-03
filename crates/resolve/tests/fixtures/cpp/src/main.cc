#include "app.hpp"

int run(int n) {
    Foo f;
    f.count = n;
    f.bar();
    return f.count + free_helper(n);
}
