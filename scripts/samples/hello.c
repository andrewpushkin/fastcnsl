#include <stdio.h>
#include <stdlib.h>

/* Print a greeting a few times. */
static void greet(const char *name, int times) {
    for (int i = 0; i < times; i++) {
        printf("Hello, %s! (%d)\n", name, i + 1);
    }
}

int main(int argc, char **argv) {
    const char *who = argc > 1 ? argv[1] : "world";
    greet(who, 3);
    return EXIT_SUCCESS;
}
