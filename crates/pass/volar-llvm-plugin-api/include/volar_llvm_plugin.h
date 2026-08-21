#ifndef VOLAR_LLVM_PLUGIN_H
#define VOLAR_LLVM_PLUGIN_H

/* This declaration must never survive to a final link: volar-llvm-plugin
 * validates and erases retained marker functions which call it. */
extern void __volar_entry(const void *target);

#if defined(__clang__)
#define VOLAR_USED __attribute__((used, noinline))
#define VOLAR_ENTRY_MARKER(name, target) \
    static VOLAR_USED void name(void) { __volar_entry((const void *)(target)); }
#else
#define VOLAR_USED
#define VOLAR_ENTRY_MARKER(name, target) \
    static void name(void) { __volar_entry((const void *)(target)); }
#endif

#endif
