#include "app.hpp"

int free_helper(int n) {
    return n + 1;
}

void Foo::bar() {
    free_helper(count);
}
