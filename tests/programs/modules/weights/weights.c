#include <stddef.h>

double total(const double* xs, size_t n) {
    double sum = 0;
    for (size_t i = 0; i < n; i++) sum += xs[i];
    return sum;
}
