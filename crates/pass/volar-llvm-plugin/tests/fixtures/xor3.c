#include "volar_llvm_plugin.h"
#include <stdio.h>
#include <stdbool.h>

bool xor3(bool a, bool b, bool c) {
    bool ab = a ^ b;
    return ab ^ c;
}

VOLAR_ENTRY_MARKER(__volar_marker_xor3, xor3)

int main(void) {
    printf("%d %d %d %d\n",
        (int)xor3(false, false, false),
        (int)xor3(true, false, false),
        (int)xor3(true, true, false),
        (int)xor3(true, true, true));
    return 0;
}
