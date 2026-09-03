#include <stdio.h>
#include "util.h"
#include "lib/api.h"

int main(void) {
    return util_helper(1) + util_inline(2);
}
