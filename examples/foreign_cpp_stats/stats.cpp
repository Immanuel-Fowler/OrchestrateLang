#include <stdint.h>
#include <cmath>

extern "C" {

double average(double a, double b, double c) {
    return (a + b + c) / 3.0;
}

double std_dev_two(double a, double b) {
    double mean = (a + b) / 2.0;
    return sqrt(((a - mean) * (a - mean) + (b - mean) * (b - mean)) / 2.0);
}

int64_t max_of_three(int64_t a, int64_t b, int64_t c) {
    if (a >= b && a >= c) return a;
    if (b >= a && b >= c) return b;
    return c;
}

}
