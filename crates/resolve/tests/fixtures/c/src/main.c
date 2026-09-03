#include <stdio.h>
#include "util.h"
#include "lib/api.h"

int main(void) {
    /* bump() has internal linkage in util.c — the linker cannot reach it from
       here, so resolution must not either. */
    return util_helper(1) + util_inline(2) + bump(3);
}
